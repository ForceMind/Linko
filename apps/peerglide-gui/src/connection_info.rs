//! Bounded public connection material for the existing explicit enrollment flow.
//! This text is unauthenticated. A certificate fingerprint identifies bytes; it
//! does not authenticate their sender. Callers must keep independent fingerprint
//! confirmation and exact TLS certificate pinning. Decoding grants no permission,
//! writes no files and never enrolls/replaces a trusted peer automatically.
use anyhow::{ensure, Result};
use peerglide_transport::{validate_certificate, MAX_CERTIFICATE_BYTES};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;

pub const MAX_TEXT_BYTES: usize = 40 * 1024;
const PREFIX: &str = "PG-CONNECTION-1:";
const SCHEMA_VERSION: u16 = 1;
const MAX_ADDRESSES: usize = 16;

/// An owned candidate snapshot, not a trust/permission token. A later import may
/// contain another certificate and requires the existing confirmation again.
pub struct ConnectionInfo {
    pub certificate: Vec<u8>,
    pub addresses: Vec<SocketAddr>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    schema_version: u16,
    input_version: u16,
    certificate_hex: String,
    addresses: Vec<SocketAddr>,
}

fn validate_addresses(addresses: &[SocketAddr]) -> Result<()> {
    ensure!(addresses.len() <= MAX_ADDRESSES, "连接地址不能超过 16 个");
    for (index, address) in addresses.iter().enumerate() {
        let SocketAddr::V4(address_v4) = address else {
            anyhow::bail!("连接地址必须使用 IPv4");
        };
        let ip = address_v4.ip();
        ensure!(
            address_v4.port() != 0
                && !ip.is_unspecified()
                && !ip.is_loopback()
                && !ip.is_multicast()
                && !ip.is_broadcast(),
            "连接地址不是允许的 IPv4 单播地址或端口无效"
        );
        // No subnet masks or machine state are inferred here. is_broadcast
        // rejects the IPv4 limited broadcast address, not guessed /24 suffixes.
        ensure!(!addresses[..index].contains(address), "连接地址不能重复");
    }
    Ok(())
}

fn validate_material(certificate: &[u8], addresses: &[SocketAddr]) -> Result<()> {
    ensure!(
        !certificate.is_empty() && certificate.len() <= MAX_CERTIFICATE_BYTES,
        "公开证书必须为 1 至 16384 字节"
    );
    validate_certificate(certificate).map_err(|_| anyhow::anyhow!("公开证书无效"))?;
    validate_addresses(addresses)
}

fn encode_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut hex = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        hex.push(char::from(DIGITS[usize::from(byte >> 4)]));
        hex.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    hex
}

fn decode_hex(hex: &str) -> Result<Vec<u8>> {
    ensure!(
        !hex.is_empty() && hex.len() <= MAX_CERTIFICATE_BYTES * 2 && hex.len().is_multiple_of(2),
        "公开证书的十六进制长度无效"
    );
    fn digit(byte: u8) -> Option<u8> {
        match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            b'A'..=b'F' => Some(byte - b'A' + 10),
            _ => None,
        }
    }
    let mut bytes = Vec::with_capacity(hex.len() / 2);
    for pair in hex.as_bytes().as_chunks::<2>().0 {
        let high = digit(pair[0]).ok_or_else(|| anyhow::anyhow!("公开证书的十六进制内容无效"))?;
        let low = digit(pair[1]).ok_or_else(|| anyhow::anyhow!("公开证书的十六进制内容无效"))?;
        bytes.push((high << 4) | low);
    }
    Ok(bytes)
}

/// Export only validated public certificate/address material, never a private
/// key, device name, permissions or a statement that the peer is authenticated.
pub fn encode(certificate: &[u8], addresses: &[SocketAddr]) -> Result<String> {
    validate_material(certificate, addresses)?;
    let envelope = Envelope {
        schema_version: SCHEMA_VERSION,
        input_version: peerglide_protocol::VERSION,
        certificate_hex: encode_hex(certificate),
        addresses: addresses.to_vec(),
    };
    let json =
        serde_json::to_string(&envelope).map_err(|_| anyhow::anyhow!("无法编码公开连接信息"))?;
    let text = format!("{PREFIX}{json}");
    ensure!(text.len() <= MAX_TEXT_BYTES, "连接信息超过 40 KiB 限制");
    Ok(text)
}

/// Parse untrusted text into an independent candidate. All errors are finite
/// messages; JSON/address/certificate parser diagnostics never echo input text.
pub fn decode(text: &str) -> Result<ConnectionInfo> {
    ensure!(text.len() <= MAX_TEXT_BYTES, "连接信息超过 40 KiB 限制");
    let json = text
        .trim()
        .strip_prefix(PREFIX)
        .ok_or_else(|| anyhow::anyhow!("连接信息标识或版本无效"))?;
    let envelope: Envelope =
        serde_json::from_str(json).map_err(|_| anyhow::anyhow!("连接信息格式无效"))?;
    ensure!(
        envelope.schema_version == SCHEMA_VERSION
            && envelope.input_version == peerglide_protocol::VERSION,
        "暂不支持此连接信息或输入协议版本"
    );
    let certificate = decode_hex(&envelope.certificate_hex)?;
    validate_material(&certificate, &envelope.addresses)?;
    Ok(ConnectionInfo {
        certificate,
        addresses: envelope.addresses,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use peerglide_transport::{fingerprint, Identity};
    use serde_json::{json, Value};

    fn certificate() -> Result<Vec<u8>> {
        Ok(Identity::generate()?.certificate)
    }
    fn address(value: &str) -> SocketAddr {
        value.parse().unwrap()
    }
    fn envelope(certificate: &[u8]) -> Value {
        json!({
            "schema_version": 1,
            "input_version": peerglide_protocol::VERSION,
            "certificate_hex": encode_hex(certificate),
            "addresses": ["192.168.1.2:4242"]
        })
    }
    fn text(value: &Value) -> String {
        format!("{PREFIX}{value}")
    }

    #[test]
    fn public_material_roundtrips_and_empty_addresses_are_a_certificate_return_card() -> Result<()>
    {
        let cert = certificate()?;
        let addresses = [address("192.168.1.2:4242"), address("169.254.2.3:65535")];
        let encoded = encode(&cert, &addresses)?;
        assert!(encoded.starts_with(PREFIX));
        assert!(!encoded.contains('\n'));
        assert!(encoded.len() <= MAX_TEXT_BYTES);
        let decoded = decode(&format!("\n {encoded}\r\n"))?;
        assert_eq!(decoded.certificate, cert);
        assert_eq!(decoded.addresses, addresses);
        let card = decode(&encode(&cert, &[])?)?;
        assert_eq!(card.certificate, cert);
        assert!(card.addresses.is_empty());
        let keys: Value = serde_json::from_str(encoded.strip_prefix(PREFIX).unwrap())?;
        assert_eq!(
            keys.as_object().unwrap().len(),
            4,
            "only the four public fields are exported"
        );
        Ok(())
    }

    #[test]
    fn original_byte_limit_is_inclusive_and_checked_before_whitespace_or_parsing() -> Result<()> {
        let encoded = encode(&certificate()?, &[])?;
        let at_limit = format!("{encoded}{}", " ".repeat(MAX_TEXT_BYTES - encoded.len()));
        assert_eq!(at_limit.len(), MAX_TEXT_BYTES);
        assert!(decode(&at_limit).is_ok());
        assert!(decode(&(at_limit + " ")).is_err());
        assert!(
            decode(&"é".repeat(MAX_TEXT_BYTES / 2 + 1)).is_err(),
            "limit counts UTF-8 bytes"
        );
        Ok(())
    }

    #[test]
    fn address_count_duplicates_and_disallowed_families_are_rejected_on_both_paths() -> Result<()> {
        let cert = certificate()?;
        let sixteen: Vec<_> = (1..=16)
            .map(|last| address(&format!("10.0.0.{last}:1")))
            .collect();
        assert_eq!(decode(&encode(&cert, &sixteen)?)?.addresses, sixteen);
        let mut too_many = sixteen;
        too_many.push(address("10.0.0.17:1"));
        let mut external = envelope(&cert);
        external["addresses"] = json!(too_many);
        assert!(encode(&cert, &too_many).is_err());
        assert!(decode(&text(&external)).is_err());
        for invalid in [
            "127.0.0.1:4242",
            "127.1.2.3:4242",
            "0.0.0.0:4242",
            "224.0.0.1:4242",
            "239.255.255.250:1900",
            "255.255.255.255:4242",
            "192.168.1.2:0",
            "[::1]:4242",
            "[::ffff:192.168.1.2]:4242",
        ] {
            external["addresses"] = json!([invalid]);
            assert!(encode(&cert, &[address(invalid)]).is_err());
            assert!(decode(&text(&external)).is_err());
        }
        let duplicate = [address("10.0.0.1:1"), address("10.0.0.1:1")];
        external["addresses"] = json!(duplicate);
        assert!(encode(&cert, &duplicate).is_err());
        assert!(decode(&text(&external)).is_err());
        // Different ports are distinct endpoints; no machine/network identity
        // or trust decision is inferred from sharing an IP address.
        let ports = [address("10.0.0.1:1"), address("10.0.0.1:2")];
        assert!(decode(&encode(&cert, &ports)?).is_ok());
        Ok(())
    }

    #[test]
    fn future_or_other_schema_and_input_versions_are_not_imported() -> Result<()> {
        let cert = certificate()?;
        for (field, version) in [
            ("schema_version", 0),
            ("schema_version", 2),
            ("input_version", 0),
            ("input_version", u64::from(peerglide_protocol::VERSION) + 1),
        ] {
            let mut external = envelope(&cert);
            external[field] = json!(version);
            assert!(decode(&text(&external)).is_err());
        }
        let current = text(&envelope(&cert));
        assert!(decode(&current.replacen(PREFIX, "PG-CONNECTION-2:", 1)).is_err());
        Ok(())
    }

    #[test]
    fn private_unknown_missing_or_duplicate_fields_and_trailing_documents_are_rejected(
    ) -> Result<()> {
        let cert = certificate()?;
        for field in [
            "private_key_hex",
            "private_key",
            "permissions",
            "device_name",
            "unexpected",
        ] {
            let mut external = envelope(&cert);
            external[field] = json!("external-private-marker");
            let error = decode(&text(&external)).err().unwrap();
            assert!(!format!("{error:#}").contains("external-private-marker"));
            assert!(!format!("{error:#}").contains(field));
        }
        for field in [
            "schema_version",
            "input_version",
            "certificate_hex",
            "addresses",
        ] {
            let mut external = envelope(&cert);
            external.as_object_mut().unwrap().remove(field);
            assert!(decode(&text(&external)).is_err());
        }
        let current = text(&envelope(&cert));
        let duplicate = current.replacen('{', "{\"schema_version\":1,", 1);
        assert!(decode(&duplicate).is_err());
        assert!(decode(&(current + "{}")).is_err());
        Ok(())
    }

    #[test]
    fn malformed_hex_invalid_der_and_certificate_size_are_rejected() -> Result<()> {
        let cert = certificate()?;
        for invalid in ["", "0", "00gg", "0x00", "00 0", "éé", "0000"] {
            let mut external = envelope(&cert);
            external["certificate_hex"] = json!(invalid);
            assert!(decode(&text(&external)).is_err());
        }
        let mut external = envelope(&cert);
        external["certificate_hex"] = json!(encode_hex(&cert).to_uppercase());
        assert_eq!(decode(&text(&external))?.certificate, cert);
        for invalid in [
            Vec::new(),
            vec![0; MAX_CERTIFICATE_BYTES],
            vec![0; MAX_CERTIFICATE_BYTES + 1],
        ] {
            assert!(encode(&invalid, &[]).is_err());
            external["certificate_hex"] = json!(encode_hex(&invalid));
            assert!(decode(&text(&external)).is_err());
        }
        Ok(())
    }

    #[test]
    fn malformed_addresses_and_parser_errors_do_not_echo_untrusted_contents() -> Result<()> {
        let cert = certificate()?;
        for invalid in [
            "untrusted-private-marker:4242",
            "999.1.1.1:4242",
            "192.168.1.2",
            "https://192.168.1.2:4242",
            "192.168.1.2:65536",
        ] {
            let mut external = envelope(&cert);
            external["addresses"] = json!([invalid]);
            let error = decode(&text(&external)).err().unwrap();
            assert!(!format!("{error:#}").contains(invalid));
        }
        for invalid in [
            "",
            "PG-CONNECTION-1:untrusted-private-marker",
            "PG-CONNECTION-1:[]",
            "PG-CONNECTION-1:null",
            "PG-CONNECTION-1:{\"private_key\":\"untrusted-private-marker\"}",
        ] {
            let error = decode(invalid).err().unwrap();
            assert!(!format!("{error:#}").contains("untrusted-private-marker"));
        }
        Ok(())
    }

    #[test]
    fn replacement_certificate_is_a_distinct_snapshot_requiring_callers_confirmation_again(
    ) -> Result<()> {
        let original = certificate()?;
        let replacement = certificate()?;
        assert_ne!(original, replacement);
        let addresses = [address("192.168.1.2:4242")];
        let first = decode(&encode(&original, &addresses)?)?;
        let confirmed = fingerprint(&first.certificate);
        let replaced = decode(&encode(&replacement, &addresses)?)?;
        assert_ne!(
            fingerprint(&replaced.certificate),
            confirmed,
            "the codec must not preserve a previous certificate's confirmation identity"
        );
        assert_eq!(
            first.certificate, original,
            "the first candidate is an owned snapshot"
        );
        assert_eq!(replaced.certificate, replacement);
        assert_eq!(replaced.addresses, addresses);
        Ok(())
    }
}
