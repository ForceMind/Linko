//! Standard OPAQUE API only; no application MAC, KDF or cipher construction.
use super::Binding;
use anyhow::{Context, Result};
use opaque_ke::{
    argon2::{Algorithm, Argon2, Params, Version},
    CipherSuite, ClientLogin, ClientLoginFinishParameters, ClientRegistration,
    ClientRegistrationFinishParameters, CredentialFinalization, CredentialRequest,
    CredentialResponse, Identifiers, ServerLogin, ServerLoginParameters, ServerRegistration,
    ServerSetup,
};
use opaque_ke::rand::{rand_core::UnwrapErr, rngs::SysRng};

fn rng() -> UnwrapErr<SysRng> {
    UnwrapErr(SysRng)
}

pub(super) struct Suite;
impl CipherSuite for Suite {
    type OprfCs = opaque_ke::Ristretto255;
    type KeyExchange = opaque_ke::TripleDh<opaque_ke::Ristretto255, sha2::Sha512>;
    type Ksf = Argon2<'static>;
}

#[cfg_attr(test, derive(Clone))]
pub(super) struct ServerState {
    state: ServerLogin<Suite>,
    binding: Binding,
}

fn ksf() -> Result<Argon2<'static>> {
    let params =
        Params::new(64 * 1024, 3, 1, None).map_err(|_| anyhow::anyhow!("invalid fixed KSF"))?;
    Ok(Argon2::new(Algorithm::Argon2id, Version::V0x13, params))
}
fn identifiers(binding: &Binding) -> Identifiers<'_> {
    Identifiers {
        client: Some(&binding.client),
        server: Some(&binding.server),
    }
}
fn parameters(binding: &Binding) -> ServerLoginParameters<'_, '_> {
    ServerLoginParameters {
        context: Some(&binding.context),
        identifiers: identifiers(binding),
    }
}

pub(super) fn client_start(code: &[u8]) -> Result<(ClientLogin<Suite>, Vec<u8>)> {
    let start = ClientLogin::<Suite>::start(&mut rng(), code)
        .map_err(|_| anyhow::anyhow!("pairing authentication failed"))?;
    Ok((start.state, start.message.serialize().to_vec()))
}

pub(super) fn server_start(
    code: &[u8],
    binding: &Binding,
    bytes: &[u8],
) -> Result<(ServerState, Vec<u8>)> {
    let request = CredentialRequest::<Suite>::deserialize(bytes)
        .map_err(|_| anyhow::anyhow!("invalid pairing request"))?;
    // opaque-ke's field-prefix decoder must not admit trailing bytes.
    anyhow::ensure!(
        request.serialize().as_slice() == bytes,
        "noncanonical pairing request"
    );
    let setup = ServerSetup::<Suite>::new(&mut rng());
    let registration = ClientRegistration::<Suite>::start(&mut rng(), code)
        .map_err(|_| anyhow::anyhow!("pairing authentication failed"))?;
    let response =
        ServerRegistration::<Suite>::start(&setup, registration.message, &binding.context)
            .map_err(|_| anyhow::anyhow!("pairing authentication failed"))?;
    // The displaying endpoint knows the short code. Registration stays entirely
    // within this process, with standard OPAQUE identifiers and fixed Argon2id.
    let ksf = ksf()?;
    let mut registered = registration
        .state
        .finish(
            &mut rng(),
            code,
            response.message,
            ClientRegistrationFinishParameters::new(identifiers(binding), Some(&ksf)),
        )
        .map_err(|_| anyhow::anyhow!("pairing authentication failed"))?;
    use zeroize::Zeroize;
    registered.export_key.zeroize();
    let record = ServerRegistration::<Suite>::finish(registered.message);
    let login = ServerLogin::<Suite>::start(
        &mut rng(),
        &setup,
        Some(record),
        request,
        &binding.context,
        parameters(binding),
    )
    .map_err(|_| anyhow::anyhow!("pairing authentication failed"))?;
    Ok((
        ServerState {
            state: login.state,
            binding: binding.clone(),
        },
        login.message.serialize().to_vec(),
    ))
}

pub(super) fn client_finish(
    state: ClientLogin<Suite>,
    code: &[u8],
    binding: &Binding,
    bytes: &[u8],
) -> Result<Vec<u8>> {
    let response = CredentialResponse::<Suite>::deserialize(bytes)
        .map_err(|_| anyhow::anyhow!("invalid pairing response"))?;
    anyhow::ensure!(
        response.serialize().as_slice() == bytes,
        "noncanonical pairing response"
    );
    let ksf = ksf()?;
    let mut finish = state
        .finish(
            &mut rng(),
            code,
            response,
            ClientLoginFinishParameters::new(
                Some(&binding.context),
                identifiers(binding),
                Some(&ksf),
            ),
        )
        .map_err(|_| anyhow::anyhow!("pairing authentication failed"))?;
    use zeroize::Zeroize;
    finish.session_key.zeroize();
    finish.export_key.zeroize();
    Ok(finish.message.serialize().to_vec())
}

pub(super) fn server_finish(state: ServerState, binding: &Binding, bytes: &[u8]) -> Result<()> {
    // TripleDh's finish verifies its stored expected MAC; its extra context/
    // identifier parameters are unused. Guard our own immutable-state contract.
    anyhow::ensure!(
        state.binding.context == binding.context
            && state.binding.client == binding.client
            && state.binding.server == binding.server,
        "pairing context changed before finalization"
    );
    let message = CredentialFinalization::<Suite>::deserialize(bytes)
        .map_err(|_| anyhow::anyhow!("invalid pairing finalization"))?;
    anyhow::ensure!(
        message.serialize().as_slice() == bytes,
        "noncanonical pairing finalization"
    );
    let mut finish = state
        .state
        .finish(message, parameters(binding))
        .map_err(|_| anyhow::anyhow!("pairing authentication failed"))?;
    use zeroize::Zeroize;
    finish.session_key.zeroize();
    Ok(())
}

// Only one fixed-cost KSF may run at a time in this process. The permit moves
// INTO the blocking task: cancelling its async waiter cannot release it early.
static CPU: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);
pub(super) async fn compute<T: Send + 'static>(
    task: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    let permit = CPU
        .acquire()
        .await
        .context("pairing computation unavailable")?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        task()
    })
    .await
    .context("pairing computation stopped")?
}
