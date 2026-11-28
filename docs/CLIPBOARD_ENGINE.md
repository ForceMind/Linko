# D01/P01 offline text collaboration engine

Implemented in peerglide-transfer::clipboard and data_policy. No external
dependency, input/ALPN v5 change, OS clipboard polling, network adapter or GUI
toggle is introduced. It is executable Rust logic with an injectable backend,
not completed cross-computer clipboard support.

## Session and consent boundary

DataSession is constructed only after the adapter independently verifies the TLS
peer pin. Its peer fingerprint and fresh cryptographically random session nonce
are local bindings, not authentication. Never accept these from remote JSON or
copy test fixtures into production. A reconnect creates a new object/nonce and
starts with zero permissions. Input permission implies no text/file permission.

Text send, text receive, file send and file receive are independent and default
off. Only explicit local consent may replace them. Consent is limited to one hour;
every replacement/revocation invalidates outstanding local tickets. Recheck a
ticket at each side effect, including after queueing. Exact expiry is denied.
Clock regression closes the session; disconnect is terminal. These are in-process
tickets, not serializable remote grants or a persistent authorization service.

## Text engine

- Maximum 1 MiB UTF-8, no replacement of invalid encoding. Empty/NUL-containing
  input is rejected (there is no remote-clear operation).
- Classification must explicitly allow sharing; unknown/sensitive/native password
  formats stay local. This cannot detect secrets embedded in ordinary plain text.
- Imported snapshots must retain provenance at the OS adapter. A 32-entry/5-second
  BLAKE3 digest cache is fallback duplicate/echo suppression, not indefinite
  provenance. The cache stores no text and expires by local monotonic time.
- Ten updates/second per direction; bounded digest cache and sequence counters.
  High-water marks deny stale/out-of-order updates. Frames must be bounded before
  allocation in the future transport, not only after parsing.
- An outbound candidate carries an opaque consent ticket; revalidate immediately
  before transmission and consume it once. Caller owns bounded queueing and
  actual send outcomes. No retry/exactly-once claim across processes.
- Receive captures the destination clipboard revision when the offer is queued.
  Backend compare-and-write checks it before writing. Changed revision skips
  the update; failure/uncertain submission consumes the sequence to prevent replay.
  A native backend must state its OS CAS limitations and preserve user changes.
- No Debug for payload-bearing types, no content/hash logging, persistence or upload.
  This is not secure-memory zeroization; a normal String exists during the operation.

## Verification and remaining implementation

Tests cover Unicode, max-size boundaries, malformed UTF-8, NUL/empty text,
wrong direction/session, queued-ticket revocation, expiry, invalid consent,
clock rollback, stale sequences, backend failures/uncertain outcomes, local
revision conflict, imported provenance, echo and event storms. Candidate native
CI results are recorded in the PR/VALIDATION after they actually finish.

Still required: authenticated data streams separated from input, bounded decoder
and backpressure, OS clipboard backends with native sensitivity/provenance/revision
support, consent/revocation UI, suspend/session lifecycle, image format validation,
cross-host/native privacy and usability acceptance. Do not enable this engine from
the input grant or advertise a working clipboard button until those paths exist.
File permissions are reusable policy primitives; they are not yet connected to
the existing offline Receiver's disk effects. See TRANSFER_STORAGE.md and #6.


## Windows 文本后端

原生后端只在明确调用时访问剪贴板。它枚举允许的纯文本格式，遇到
任何未知或私有格式则保持本机。读取限制 UTF-16 内存和最终 UTF-8
长度，并核对读取前后的版本。接收端先备好自己的可移动内存，再
持有剪贴板锁核对原版本、清空并提交。清空成功后若提交失败，返回
结果未知，不自动重试。只记录本次导入的版本用于来源判定。
CI 使用纯数据回归并编译原生接口，系统实际读写留到集中验收。

- [OpenClipboard](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-openclipboard)
- [GetClipboardSequenceNumber](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getclipboardsequencenumber)
- [SetClipboardData](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setclipboarddata)


## macOS 文本后端

后端由 AppKit 主线程持有，只按明确调用读取。先枚举有界纯文本类型，
遇到未知、私有或隐藏标记则保留本机。读取前后核对 pasteboard
`changeCount`。写入准备完毕后再核对预期版本，清空后复核当前归属，
提交后也检查版本。公开 API 将版本查询、清空和写入分开，其他进程
仍可能在相邻操作间修改剪贴板。清空后出现竞争或失败即返回结果
未知，禁止自动重试。真实竞争和系统读写留到集中验收。

- [Apple NSPasteboard changeCount](https://developer.apple.com/documentation/appkit/nspasteboard/changecount)
