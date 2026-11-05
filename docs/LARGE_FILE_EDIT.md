# 大文件本地编辑：阶段一

V2 读取、快照及保存统一允许至 1 GiB。配套 Server 必须包含 1 GiB 修改，
否则旧服务仍拒绝大于 128 MiB 的文件；客户端不会绕过其授权或存储入口。
HTTP 413 显示大小限制，保留工作文件；传输总超时调整为一小时、连接超时为 15 秒。

Journal 保持 64 KiB 有界快照复制。旧 CDC 仍只允许原 128 MiB 内存预算，
它不是 V2 大文件路径，不因公共上限提高而加载 1 GiB 到内存。

验证：cargo fmt、workspace tests、workspace clippy；额外以 128 MiB+1 的真实
快照及 1 GiB+1 的稀疏文件验证接受和拒绝。未执行生产/Windows/CAD 实机验收。
后续 CDC 应使用当前 V2 ticket、Journal 和最终原生版本提交，而非旧设备协议。
