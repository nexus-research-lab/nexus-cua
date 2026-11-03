# Nexus Computer Use Runtime 完整产品与开发 PRD

- 状态：`0.2.0-rc.1` macOS 测试预发布基线
- 语言：中文
- 适用范围：`nexus-cua` 仓库
- 最后更新：2026-11-03（接管、低复制复核与动作后观察）

本文档是 `nexus-cua` 的唯一产品级入口，统一说明产品定位、系统结构、
能力范围、使用方式、安装与集成、权限模型、性能目标、CI/CD、验收标准和
开发顺序。协议字段、运行时细节和平台证据仍由专门的技术文档承载，但不得
与本 PRD 的产品决策冲突。

> **2026-11-02 设计修订说明。** 真实使用暴露出三个根本问题：用户看不到 Agent
> 正在观察/操作（无录屏指示、无操作反馈）；Agent 经常"看不到 app"（无窗口应用、
> 其他桌面空间、全屏、弹出菜单都在观察之外）；多显示器难以区分。它们源于本 PRD
> 早期"只观察单个精确窗口、一次性截图、显示器只读概览"的设计，而不是个别实现
> 缺陷。§2.5–§2.7 给出修订后的核心决策，§23 给出完整审查、根因、解决方案和
> 验收条件。凡本 PRD 与 [Architecture](architecture.md)、
> [Protocol v1](specs/protocol-v1.md)、[Runtime Contract v1](specs/runtime-contract-v1.md)
> 中"exact-window-only 观察"相关表述冲突，以本 PRD 为准；技术文档在对应实现
> 落地的同一变更中同步修改。

## 1. 一句话定义

Nexus Computer Use Runtime 是一个模型无关、跨平台、权限受控的桌面执行层。
它让 Nexus、Codex 和其他 Agent 通过同一套协议观察并操作 macOS 或 Windows
上的真实桌面应用，但它本身不是 Agent，不负责模型推理、规划、记忆或聊天。

## 2. 核心产品决策

### 2.1 一个 Runtime，两个适配器

产品只有一个后端：`nexus-cua runtime`。

```text
                         ┌─ MCP adapter ─ MCP 绑定 Skill
统一动作与能力协议 ─────┤
                         └─ CLI adapter ─ CLI 绑定 Skill
                                  │
                         nexus-cua runtime
                                  │
                    macOS / Windows 原生驱动
```

MCP 和 CLI 只是两种薄入口。共同操作必须具有相同的参数语义、权限规则、
会话生命周期、错误码、动作证据、重试规则和平台实现，不能发展成两个后端。

### 2.2 一个 Skill 源，每个宿主只安装一种绑定

仓库只维护一份公共 Skill 内容和一份统一动作目录，然后生成：

- MCP 绑定 Skill：只描述 MCP tools；
- CLI 绑定 Skill：只描述 `nexuscua` 命令。

一个宿主内不能让模型同时看到两种接口。Skill 不执行接口探测，也不写
“MCP 失败后改用 CLI”之类的回退逻辑。接口选择发生在安装阶段，由宿主或
安装器决定。

### 2.3 Nexus 和通用生态使用同一发行物

- Nexus UI 或 `nexuscfg` 安装：选择 Nexus profile，启用 CLI 绑定 Skill；
- Codex 或其他 Agent 插件安装：选择 MCP profile，启用 MCP 绑定 Skill；
- Homebrew、WinGet、npm 或压缩包裸安装：选择 unbound profile，不修改任何
  宿主配置，也不向未知 Skill 目录写文件。

三个 profile 安装的是同一版本、同一 Runtime 和同一动作协议。

### 2.4 VM 不是默认前提

默认模式直接控制用户当前的真实桌面，因此可以看到用户在 Finder、资源管理器
或桌面上实际能看到的文件和窗口。VM 是可选环境 provider，只用于并行独立桌面、
不可信任务、可复现环境和回滚，不是本地 CUA 的前置条件。

### 2.5 看得见场景，只动被授权的应用（2026-11 修订）

原设计把"观察单位"和"授权单位"都定为单个精确窗口：模型只能看到一个窗口
裁剪图，显示器截图只读且需另行授权。结果是右键菜单、下拉菜单、自动补全、
sheet 之外的独立对话框、tooltip 这些属于同一应用但不在主窗口内的 UI 全部不可见，
多窗口应用要逐个切换，模型也无法理解目标窗口在哪块屏幕、被什么遮挡。

修订后把两者拆开：

- **观察单位 = 应用场景（application scene）**：目标应用所在显示器的整屏画面，
  但只渲染被授权应用（及其被允许的系统辅助进程，例如打开/保存面板服务）的
  全部窗口，包括菜单、弹窗、popover、tooltip；其他应用区域被遮盖。macOS 使用
  ScreenCaptureKit 的 display + including-applications 过滤器原生实现；Windows 使用
  Windows.Graphics.Capture 显示器捕获加基于 z-order 可见区域的软件遮盖。
- **授权单位 = 应用（及其窗口）**：像素动作在派发前对目标点做 hit-test，命中的
  最上层窗口必须属于被授权应用或其允许的辅助进程，否则 `not_dispatched`。
  语义动作仍绑定 observation 内的 element ref。
- 精确窗口观察（`observe_window`）保留，用于需要小图、高精度或后台遮挡场景；
  整屏无遮盖的显示器观察仍是独立 read authority，只用于用户明确允许的全局概览。
- 隐私不因此放宽：未授权应用的像素不进入截图，也不进入 artifact、日志或指纹。

### 2.6 用户始终知道 Agent 在看、在动（2026-11 修订）

真实桌面 CUA 的第一产品要求是"可感知、可打断"。原设计使用一次性截图且没有
任何操作反馈，用户既看不到系统录屏指示，也不知道 Agent 下一步点哪里。对照
用户实测：Codex 的 Computer Use 会触发系统屏幕录制指示，本项目不会。

修订后的要求：

1. **系统级指示**：session 处于活动观察期时，Runtime 为目标显示器保持一个有界的
   捕获流（macOS `SCStream`，Windows WGC 并保留系统黄色边框），使操作系统的
   录屏/捕获指示持续可见；该流同时作为 warm capture pipeline 提升性能，空闲超时
   或 session 关闭即停止。
2. **产品级指示**：Runtime 通过 presence 事件告诉宿主"正在观察哪个应用""即将在
   哪个点执行什么动作"；宿主渲染目标应用高亮边框、顶部 HUD（应用名 + 当前动作 +
   停止按钮）、Agent 光标/点击涟漪。所有 overlay 必须从捕获中排除（macOS 过滤器
   excepting windows，Windows `WDA_EXCLUDEFROMCAPTURE`），且鼠标穿透。
3. **紧急停止**：全局快捷键与 HUD 停止按钮立即关闭 admission 并撤销 session；
   已 admitted 的动作如实报告，不谎称回滚。
4. **用户接管**：macOS 两次原生动作之间的输入保留原授权，等待连续空闲 2 秒
   （单次最多 5 秒），要求重新观察；持续活动返回 `busy/not_dispatched`。
   原生动作执行期间的输入返回并锁定 `user_active`，可能已执行的部分如实报告
   `indeterminate`，需用户明确继续后重新授权。停止、到期和监听失效不自动恢复。
5. 独立 MCP/Codex 路径没有宿主 UI 时，`nexuscua` 自带最小指示（菜单栏/托盘状态项 +
   目标边框 + 停止项），不能因为没有宿主就静默操作。

### 2.7 一步一循环的模型交互（2026-11 修订）

原设计每个 observation 只能驱动一个动作，且每次动作前要整幅重截图做校验，
模型每一步至少两次往返；Nexus 路径又叠加了 30 秒观察有效期和逐动作人工审批，
审批稍慢动作就因 stale 失败。修订后：

- 动作默认返回动作后的场景观察（`observe_after`，带有界的界面稳定等待），
  一次往返完成"动作 + 新画面"；
- 所有适配器（MCP、CLI、Nexus、SDK helper）统一使用左上原点 `0..1000` 归一化
  坐标，归一化映射下沉到协议层，不再各自实现；
- 截图在捕获阶段按宿主给定的模型图像预算（例如长边上限）缩放，不再把物理
  分辨率原图交给模型；
- 观察有效期由宿主统一配置，所有入口默认一致；人工审批等待时间不计入有效期，
  审批通过后先做轻量视觉复核，变化时直接返回新观察供模型重新决策；
- 授权在"选择目标"时一次性确认（应用、动作类别、前台输入、时长），只有高风险
  动作（退出应用、剪贴板写入、宿主策略标记的动作）逐次确认。

## 3. 产品目标

`0.1.x` 的目标是：

1. 完成 macOS 与 Windows x64 的原生观察和动作能力；
2. 对齐 trycua 中适合本项目边界的关键能力；
3. 支持真实桌面、后台能力、多 Agent 会话、多桌面 worker 和远程传输；
4. 提供独立 `nexuscua` CLI、MCP Server 和生成式 Skill；
5. 针对 Kimi、GLM、DeepSeek 派生视觉方案提供低复杂度工具 schema；
6. 保持严格的会话权限、目标绑定、新鲜观察、防重放和隐私规则；
7. 控制常驻内存、截图缓存、队列、并发、重试和空闲 CPU；
8. 建立 Windows x64、macOS、签名、SBOM、校验和、来源证明和干净机器验证；
9. 最终由真实 Codex 通过 MCP 完成端到端验收；
10. 完成候选产物但暂不发布，八小时 soak 延后到正式发布阶段。

## 4. 非目标与永久边界

本仓库不负责：

- 模型调用、模型路由、Agent loop、规划、记忆或任务完成判断；
- Nexus 用户、会话、聊天、订阅、偏好、审批 UI 和业务审计对象；
- 浏览器 DOM、CDP、标签页、历史记录或网络请求；
- 默认保存截图、无障碍树、剪贴板、输入文本或轨迹历史；
- 让 Skill、MCP 连接、CLI 进程、OS 权限或本地用户身份自动等同于授权；
- 通过 Runtime 或 Driver 下载可执行代码、自更新或加载第三方 Driver；
- 在真实用户桌面上伪造 snapshot、reset、clone；
- 为了表面功能完整而使用未公开的系统 API 作为静默回退；
- 直接修改 Nexus `main` 主分支；下游集成必须在独立 branch/worktree 进行；
- 在当前阶段发布任何正式版本。

## 5. 名称、版本和公开入口

### 5.1 名称

| 对象 | 目标名称 |
| --- | --- |
| 仓库 | `nexus-cua` |
| 产品 | Nexus Computer Use Runtime |
| 公共 CLI | `nexuscua` |
| MCP 启动入口 | `nexuscua mcp serve`；不发布第二个 MCP 可执行文件 |
| MCP 推荐注册 alias | `nexuscua` |
| MCP handshake `implementation.name` | `nexuscua` |
| MCP 原始 tool 名 | 无产品前缀的稳定 leaf，例如 `observe_window` |
| 宿主限定 tool 名 | `mcp__nexuscua__observe_window` |
| 协议 | `nexus.cua.v1` |
| Python distribution | `nexus-cua` |
| TypeScript SDK | `@nexus-research-lab/nexus-cua` |

这几层名称不能混用：启动命令是进程入口，注册 alias 是宿主配置键，handshake
identity 是 MCP 自报实现身份，tool leaf 是 server 内部操作名，最终限定名由宿主按
`mcp__<alias>__<leaf>` 生成。tool leaf 不得再携带 `nexus_cua_`，否则会产生
`mcp__nexuscua__nexus_cua_observe_window` 这类双重命名。

选择 `nexuscua` 而不是 `nexus_cua` 作为推荐 alias 有两个原因：它与公共 CLI 一致；
Nexus 把 `nexus_*` 保留给内建 MCP，外部自定义 MCP 使用 `nexus_cua` 会被拒绝。
受管的 Codex/MCP integration 必须注册精确 alias `nexuscua`；若该名称已被其他 server
占用则 fail closed，不得自动追加数字，否则生成 Skill 中的限定工具名会漂移。手工宿主
可以选择其他 alias，但必须自行生成对应 binding，不能继续使用官方 `nexuscua` Skill。

Nexus 产品内建 Computer Use 不挂载该 MCP；它使用 `computer-use` Skill、round-scoped
`nexus computer` CLI 和私有 broker。`nexuscua` MCP 只服务 Codex、其他通用 MCP Host，
以及用户主动选择的 Nexus 外部自定义 MCP 路径。

当前源码已经一次性切换为公共命令 `nexuscua`、MCP handshake identity `nexuscua`
和无前缀 tool leaf；独立 `nexus-cua-mcp` 二进制与 `nexus_cua_*` raw tools 已删除。
项目尚未发布，因此没有兼容 alias、deprecated wrapper、双 tool 发布或旧配置迁移。
内部 Cargo package 名仍可使用 `nexus-cua-*`，但不能暴露成第二套产品入口。

### 5.2 版本规则

- Runtime、CLI、MCP、生成 Skill 和安装 manifest 使用同一产品版本；
- wire protocol 单独声明兼容范围；
- Nexus 和插件宿主必须固定精确版本，不使用不可追溯的 `latest`；
- 升级采用并排 staging、健康检查和原子切换；
- 回滚到上一个已验证版本；
- session、token、request ledger、artifact generation 和 worker epoch 不得跨版本继承；
- Runtime 本身永远不执行自更新。

## 6. 仓库结构

### 6.1 核心模块

| 路径/模块 | 职责 |
| --- | --- |
| `crates/protocol` | 封闭 wire 类型、opaque refs、能力、动作和稳定错误 |
| `crates/runtime` | 会话、权限验证、目标投影、新鲜度、验证和临时产物 |
| `crates/platform` | macOS 与 Windows 捕获、无障碍、发现、生命周期和输入 actor |
| `crates/transport` | 本地 Unix Socket / Windows Named Pipe、认证、帧、幂等 ledger 和 reconcile |
| `crates/host` | 有界 desktop-worker 注册、lease/epoch、远程 gateway 和环境 provider 合同 |
| `crates/mcp` | 本地 MCP adapter、图像返回、zoom、视觉模型工具 profile |
| `crates/cli` | 统一 `nexuscua` 公开入口、诊断、host-bound service、Agent 命令、MCP 与安装集成 |

### 6.2 SDK

| 路径 | 用途 |
| --- | --- |
| `sdk/go` | Nexus 等可信宿主的主要 typed client |
| `sdk/python` | Python Agent/自动化生态和 quickstart |
| `sdk/typescript` | Node.js typed client，零运行时依赖 |

三个 SDK 必须消费相同兼容性 fixtures，并保持相同的 request identity、
opaque ref、敏感值和错误语义。

### 6.3 测试与发布设施

| 路径 | 用途 |
| --- | --- |
| `schemas/nexus.cua.v1` | 提交到仓库的 request/response JSON Schema |
| `fixtures/compatibility` | 跨语言 wire 兼容语料 |
| `fixtures/native` | macOS/Windows 确定性 GUI fixture |
| `tools/native-harness` | 原生功能、故障、权限、性能、soak、重启和证据聚合 |
| `benchmarks/runners` | 维护硬件 runner 的固定描述和基线 |
| `scripts/release_candidate.py` | 候选版本、证据和 manifest 的 fail-closed 校验 |
| `scripts/package_channels.py` | 从 accepted canonical payload 生成 npm/Homebrew/WinGet 渠道输入 |
| `profiles` | unbound/Nexus/MCP 的闭合、非自动激活安装 profile |
| `packaging` | 渠道名称、npm meta launcher 与无发布副作用的打包源 |
| `.github/workflows` | CI、维护硬件证据和不发布的候选产物流水线 |

## 7. 运行架构与信任边界

### 7.1 本地 Runtime

```text
可信 Host
  │  创建权限、选择应用、设置 TTL、保存审计
  ▼
本地私有 IPC
  │  Unix Socket 0600 / owner-only Windows Named Pipe
  ▼
nexus-cua runtime
  │  校验 session、target、observation、action、deadline
  ▼
平台 actor
  │  AX run loop / COM apartment / capture queue / input lane
  ▼
精确应用与窗口
```

Runtime 不接受 PID、HWND、AX 对象、COM 对象、任意可执行路径或其他 native
handle 作为公共权限。应用、窗口、显示器、session、observation、element、
artifact、worker 和 snapshot 都使用 runtime 生成的 opaque refs。

### 7.2 Nexus 路径

```text
Nexus Agent Runtime
  │  内置 computer-use Skill 调用 round-scoped nexus.command
  ▼
Nexus Host / Go service
  │  owner、round、审批、receipt、disable 与生命周期
  ▼
Pinned Go SDK client + supervisor
  │  私有 Unix socket、一次性 token、私有 artifact root
  ▼
私有 nexus-cua runtime
  ▼
macOS / Windows 原生驱动
```

Nexus 下游集成已在独立的最新 `main` branch/worktree 开始实现；它不挂载
`nexuscua` MCP，也不把 endpoint、token、native handle 或截图路径暴露给 Agent。
UI/`nexuscfg` 安装、签名 payload、clean-host 生命周期和真实 Codex acceptance
仍按 M7 计划逐项关闭。

### 7.3 通用 MCP 路径

```text
Codex / MCP Host
  │  MCP 绑定 Skill
  ▼
MCP adapter（stdio / http 二选一）
  │  已认证 authority provider
  ▼
嵌入式或本地 nexus-cua runtime
  ▼
原生桌面
```

本地默认使用 stdio，由宿主按需启动，不占端口。独立服务或多客户端场景显式选择
Streamable HTTP。HTTP 默认绑定 loopback，并让 OS 通过 `port=0` 分配空闲端口，再把
endpoint 写入 owner-private registration；不得猜一个所谓“不容易冲突”的固定端口，
也不得在 transport 之间静默切换。SSE 只是 Streamable HTTP 可返回的流式响应形式，
不是第三种配置，也不兼容已废弃的旧 HTTP+SSE wire。

### 7.4 Authority bootstrap

安装成功不等于获得桌面权限。生产 MCP 和 CLI adapter 必须从已注册的可信
authority provider 获取授权：

1. adapter 通过继承的 native handle 或 owner-private broker 建立认证连接；
2. adapter 发送本次进程实例 nonce；
3. provider 返回绑定 nonce、runtime/worker epoch、principal/work-scope、
   allowed operations、request namespace 和短 TTL 的认证 grant；
4. adapter 验证后才发布或执行桌面工具；
5. grant 不得通过普通 argv、环境变量、stdout、日志或模型可读配置传递。

M3 之后若开发环境仍保留 authority file fallback，该文件必须由注册 provider 签名
或 MAC，
位于模型不可见目录，并绑定同样的 nonce/epoch/scope/expiry。仅靠文件所有者和
`0600` 不足以构成生产信任根。

当前 `codex-mcp.md` 中的普通 JSON 是 M3 之前遗留的 source-build fixture
mechanics 测试，只能在模型 shell 只读、应用固定且无产品声明的隔离开发环境运行。
它不证明 authority 安全，不能计入 M0–M6 的任何验收；M3 完成后删除该例外并把
runbook 切换到认证 provider。

当前源码已实现封闭的 provider challenge/grant 合同和生产 provider source：grant
必须绑定 adapter nonce、registration、runtime/worker epoch、principal/work scope、
request namespace 与短 TTL；不匹配、过期和重放在发布工具前失败，mutation ID 必须
使用 provider namespace。普通 `--authority-file` 已删除，unsigned fixture 只能同时
显式传入 `--development-authority-file` 与
`--allow-unsigned-development-authority`。OS-backed provider client、Keychain/DPAPI
secret storage、registration MAC、精确父宿主 executable 复核、nonce replay ledger 和
`mcp launch --registration` 与 Codex stdio 的 `integration inspect/plan/apply/remove`
已实现：plan 由 OS secret 认证并限制十分钟，apply 只接受真实 TTY 的显式确认，使用
官方 `codex mcp` 入口，安装单一 MCP Skill，并在配置漂移或失败时拒绝/回滚；remove
只处理认证收据中精确匹配的自有状态。独立真人
`authority plan/apply/status/revoke` 也已实现：plan/apply 强制真实 TTY，apply 重新复核
精确宿主 executable，status 交叉验证认证收据和 registration，revoke 先写认证撤销意图
再删除 capability material，只有 registration 已不可用才报告 `revoked`。CLI profile
已有 host-bound `service start/stop/status` 以及 session/app/window/display/clipboard/action/
verify Agent 命令；内部服务复用当前 Streamable HTTP server core，但只在动态 loopback
`/cli` 上运行，capability 由 provider 内部派生，Agent 命令不接受 endpoint、token、
authority path、PID/HWND、应用路径或权限覆盖。服务在 registration 消失、绑定实例的
stop request 或 grant TTL 到期时停止，CLI revoke 会等待服务状态清理。尚缺可信宿主的
自动启动/安装、grant 续期、registered HTTP host integration，以及 stdio MCP 等其他
运行中 adapter 的完整终止编排，所以本项仍是部分完成。registration 与认证 integration
state 已分别硬限制为 128 与 512 条，安装/删除由 owner-private 跨进程文件锁串行化，
删除后才回收容量。

### 7.5 Authority provider 的安装、首个授权与撤销

`0.1.x` 必须提供一个模型无关的本地 authority provider，不要求 Codex 或独立
CLI 自己实现权限系统。provider 由 `nexuscua` 安装组件提供，Nexus 后续可以用
自己的 provider 替换它。

安装与注册流程：

1. `nexuscua integration plan` 生成 host registration、目标 profile、Skill、
   authority ceiling 和变更清单；
2. 真人在 TTY/宿主 UI 中确认，模型调用和非交互 stdin 不能确认；
3. `integration apply` 把 provider 私钥放入 macOS Keychain 或 Windows DPAPI/
   Credential Manager 保护的存储，把非秘密 registration ID 写入宿主配置；
4. provider 公钥、产品版本和允许的宿主 executable identity 写入 owner-private
   registration；
5. MCP 配置只出现类似
   `nexuscua mcp launch --registration <non-secret-id>` 的命令，不出现 grant、
   token、authority path 或 provider 私钥；
6. launcher 创建 provider 与 MCP/CLI adapter 之间的继承 handle，完成 nonce
   challenge 后发出短期 grant；
7. grant 到期前只能由同一 provider、同一 adapter instance 和同一 work scope
   续期；feature disable、registration remove、宿主退出或 provider identity
   变化立即停止续期并撤销新 admission。

首个授权的信任根是“OS 用户明确启动受信安装/授权流程并确认可见 scope”，不是
“同一 OS 用户下任意进程”。真人确认页面必须显示应用身份、read/mutation 类型、
动作集合、foreground/background、clipboard/display、TTL 和宿主名称。

独立真人 CLI 流程：

```text
nexuscua authority plan --interactive
nexuscua authority apply --plan <ref> --confirm
nexuscua session open --registration <id>
...
nexuscua session close
```

受信自动化不能使用 `--confirm` 冒充真人；它必须导入由管理员/provider 签名的
policy，通过保护 channel 获取 grant。Agent Skill 永远不包含 authority plan/apply
命令。Nexus profile 不安装本地通用 provider，而是通过 Nexus scoped broker 完成
同一 host-neutral grant contract。

撤销时必须关闭或到期所有相关 session、使旧 nonce/grant 不可复用、删除 provider
私钥与 registration，并保留不含敏感数据的撤销记录。已 admitted 的 OS mutation
可能完成；撤销不能谎称已经回滚。

## 8. 功能范围

状态含义：

- **已实现**：源码和确定性 contract tests 已存在，平台支持仍取决于维护硬件证据；
- **部分完成**：核心机制存在，但平台、宿主或完整验收仍缺失；
- **待开发**：尚无可用实现；
- **边界**：有意属于 Nexus 或独立 provider，不进入核心 Runtime。

### 8.1 应用、窗口和显示器

| 能力 | 当前状态 | 完成标准 |
| --- | --- | --- |
| 运行中应用发现 | 需修正 | 以系统运行应用列表为来源（含无窗口、已隐藏应用并标注窗口数与隐藏状态），而不是从可见窗口反推；短期 discovery ref、身份复核、无 PID authority |
| 已安装应用发现 | 已实现 | macOS bundle 与 Windows Start Menu 受控来源 |
| 冷启动应用 | 已实现 | 单次 launch ref、路径不可由模型提供、进程代际复核 |
| 激活应用 | 待开发 | 取消隐藏并切换到应用所在桌面空间/全屏空间；需要前台授权 |
| 协作退出 | 已实现 | 只终止精确进程代际，不升级为任意 kill |
| 顶层窗口枚举 | 需修正 | 窗口状态使用枚举 `normal/minimized/app_hidden/other_space/fullscreen_space/offscreen`，最小化由 AX/UIA 回读而非 `!visible` 推断；携带 `display_ref`、z-order、窗口角色（standard/dialog/sheet/panel）；exact window ref、重启和替换失效 |
| 应用场景观察 | 待开发 | 见 §2.5：目标显示器整屏 + 仅渲染授权应用全部窗口（含菜单/弹窗），其他区域遮盖；DPI 映射、受控临时产物 |
| 窗口截图 | 已实现（降级为辅助） | 精确窗口、DPI 映射、受控临时产物；不再是模型默认观察 |
| 物理显示器身份 | 需修正 | 显示器名称、序号（左→右、上→下）、缩放比例、像素尺寸、主屏、是否含光标/前台窗口、所含窗口；截图附带可读标签 |
| 物理显示器概览 | 已实现 | 独立只读 authority，不能作为动作 guard |
| 窗口聚焦 | 已实现 | 显式授权，不从失败动作静默升级 |
| 移动/缩放窗口 | 已实现 | 系统路由、fresh observation、几何验证 |
| 最小化/恢复 | 已实现 | 系统路由和状态回读；所有模型入口必须暴露恢复动作 |

### 8.2 视觉与语义观察

- PNG 截图及长度、SHA-256 和尺寸校验；
- `interactive` 与受限 `full` accessibility 快照；
- 节点数、深度、字节数和时间预算；
- 截断时显式返回 partial，而不是伪装完整；
- 密码/secure text 值必须脱敏；
- 截图像素坐标与逻辑屏幕坐标携带明确 affine mapping；
- Retina 和混合 DPI 不由模型自行推断；
- 一次成功 mutation 后旧 observation 和相关 zoom view 失效；
- zoom 是服务端有界裁剪，不是模型厂商特性。

所有视觉坐标使用左上角原点的 `0..1000` 归一化整数。模型看到缩放后的图片也
不会改变坐标含义。该规则是协议级要求：Runtime 原生接受归一化坐标，MCP、CLI、
Nexus 和 SDK 不得各自实现或绕过（2026-11 前只有 MCP 实现）。

2026-11 修订新增：

- 默认观察是 §2.5 的应用场景；观察结果附带显示器标签和授权窗口清单
  （window ref、标题、在图中的归一化矩形、状态）；
- 截图在捕获阶段按宿主给定的模型图像预算缩放（默认长边不超过 1568 像素，
  宿主可按模型调整），不对物理分辨率原图做 PNG 编码；
- 校验用指纹使用低分辨率捕获，不再在每次动作前整幅重截图；
- 动作可返回动作后观察（`observe_after`），界面稳定等待有上限。

### 8.3 原生动作

当前协议包含：

| 类别 | 动作 |
| --- | --- |
| 窗口 | focus、move/resize、minimize/restore、cooperative terminate |
| 语义 | focus element、invoke、set text value、set range value、toggle、select、expand/collapse |
| 指针 | left/middle/right click、multi-click、bounded hold、move、drag |
| 键盘 | type text、normalized key chord |
| 滚动 | 在指定点滚动（2026-11 修订：必须带坐标）、可选 semantic target |
| 剪贴板 | bounded plain-text read、fresh-ref compare-and-replace write |

跨平台语义必须一致（2026-11 审查发现多处不一致，见 §23.3）：

- 滚动单位是逻辑像素，两平台换算一致；滚动事件必须投递到指定点，且该点
  hit-test 属于授权应用，不能落在当前光标下的任意应用；
- 键名使用平台中立集合：`mod`（macOS 为 Command、Windows 为 Ctrl）、`cmd`、
  `ctrl`、`alt`/`option`、`shift`、`win`；`backspace` 与 `delete`（向前删除）语义
  两平台一致；常见别名（`ctrl`、`cmd`、`esc`、`del`）在入口归一化；
- 支持标点与符号键（`,` `.` `/` `;` `'` `[` `]` `\` `-` `=` `` ` ``），并按当前键盘
  布局把字符映射到物理键，不假设美式 ANSI 布局；
- 点击前先移动指针到目标点，按下/抬起与多击之间有平台合理间隔；文字输入按块
  节流，避免 Electron/Chromium 类应用丢字；
- 中文等输入法处于组字状态时，`press_keys` 的字母键不得被当作文本输入：
  派发前检测并报告，或由 `type_text` 路径绕过输入法。

每个新增原生动作必须同时具备：

- closed protocol variant 和 capability bit；
- host-issued authority；
- fresh observation 或操作专用 guard；
- macOS AX/CGEvent/ScreenCaptureKit 或 Windows UIA/SendInput/WGC 的正确 actor 所有权；
- `not_dispatched` 与 `indeterminate` 的准确区分；
- delivery/effect/evidence；
- 确定性 fixture 和维护硬件验证；
- 明确的时间、数量和内存边界。

### 8.4 后台能力

后台不是一个笼统开关：

- exact-window capture 在公共 API 支持时可独立于遮挡；
- AX/UIA semantic action 在可精确寻址时可以不抢前台；
- background pixel click 只在平台能如实路由时提供；
- 键盘和全局指针输入默认属于前台输入；
- background 失败不得静默升级成 foreground；
- 临时前台输入可单独授权恢复先前 foreground owner，但用户或系统已改变前台时
  必须跳过恢复。

当前 macOS background click 已有证据；Windows 和更多 raw background input
类型仍需要维护硬件验证。

### 8.5 多 session 与多 worker

- 一个 Runtime 可承载有界的多个 capability session；
- 多个 Agent worker 可以共享一个桌面；
- 一个 OS 桌面只有一条 foreground mutation lane；
- lane 忙时立即返回 `busy/not_dispatched`，不进入无界队列；
- 独立并行前台工作必须使用独立 desktop workers；
- worker 调度必须有界、能力感知，并绑定 lease 与 epoch；
- admitted 或 indeterminate mutation 不得换 worker 重放；
- worker 重启、掉线或重新注册会使旧 lease/ref 失效。

`crates/host` 已实现 worker registry、least-loaded capability placement、lease、
epoch 和 gateway 合同。本仓 M2 负责模型无关的 secure-stream framing、双端
conformance fixture 和断线/reconcile 语义；Nexus worker pool、地址发现、账户、
公网 listener 和生产 TLS/SSH/RPC 运维仍属于后续 Nexus 集成。

### 8.6 远程 observation/action

私有 sidecar 始终保持本机 IPC，不直接开公网 TCP。远程路径是：

```text
Agent -> 已认证 Host Gateway -> Desktop Worker 本地 client
      -> 本地 sidecar -> OS
```

远程请求必须携带 gateway 认证、worker lease、worker epoch 和稳定 request ID。
远程图片以短期 artifact ticket 返回，ticket 绑定 lease/epoch、长度和 SHA-256，
不能暴露 worker 本地路径。网络断开不能宣称已 admitted 的动作被取消。

M2 不新增常驻公网 listener。它必须提供一个两进程 reference harness：controller
与 desktop worker 使用调用方提供的双向安全字节流连接，测试 fixture 用相互认证的
临时 mTLS 通道实现该字节流。验收固定覆盖认证先于 worker resolution、错误证书、
lease/epoch 失效、frame 上限、artifact ticket 长度和 digest、mutation 中途断线后以
相同 `request_id` reconcile，以及重连后不得跨 epoch 重放。输出脱敏的
`nexus.cua.remote-transport.v1` summary，记录 source revision、两端 binary digest、
证书 identity digest、场景 disposition 和资源上限；不记录图片或凭据。

### 8.7 网络与文件系统策略

CUA 核心不提供直接文件读写或网络请求工具。用户通过 Finder、资源管理器或某个
应用 UI 看见文件，不等于 Agent 获得直接 filesystem authority；应用自身联网也
不等于 Agent 获得 socket authority。

worker descriptor 分别声明：

- GUI-visible application state；
- direct filesystem policy；
- direct network policy；
- isolated environment mounts。

本地桌面如实报告 `not_enforced`。只有真正拥有隔离边界的 provider 才能声明
deny-all、host allowlist、只读 mount 或 unrestricted 等策略。

### 8.8 Snapshot、reset、clone

本地用户桌面明确返回 unsupported。关闭 CUA session 只撤销权限并删除临时产物，
不会回滚文件或应用状态。

可选 VM provider 可以实现：

- snapshot；
- reset 到命名 checkpoint；
- 从 immutable base clone；
- 启动前网络与 mount 策略。

这些能力必须绑定 worker epoch 和 base image digest，并限制 clone 数、snapshot 数、
存活时间和存储量。`0.1.x` 要求 provider contract、capability negotiation 和 bounded
test provider 完整，但不要求生产 VM backend 才能完成本地产品。

### 8.9 当前明确缺口

优先级时间范围：P0 必须在本 PRD 的 M1–M6 完成；P1 是本仓完成后的下一产品
阶段，除非 M2 只要求其 contract/test provider；P2 是长期候选，不进入当前排期。

2026-11 修订：以下"设计修订"条目优先于本表其他 P0，排期见 §21 R0–R2。

| 项目 | 优先级 | 说明 |
| --- | --- | --- |
| 应用场景观察与 hit-test 授权 | P0（设计修订） | §2.5；解决"看不到菜单/弹窗/多窗口"和多屏上下文缺失 |
| 用户可感知与紧急停止 | P0（设计修订） | §2.6；系统录屏指示、presence 事件、宿主 overlay、停止快捷键、用户接管；原"synthetic per-agent cursor P2"并入此项并提升 |
| 应用/窗口/显示器发现修正 | P0（设计修订） | 无窗口应用、隐藏应用、其他桌面空间、全屏空间、显示器身份，见 §23.2 |
| 交互循环与坐标统一 | P0（设计修订） | §2.7；observe_after、协议级归一化坐标、捕获阶段缩放、统一观察有效期 |
| 跨平台输入一致性缺陷 | P0（缺陷） | 滚动坐标与单位、键名语义、键表与布局、点击/输入节奏，见 §23.3 |
| Windows x64 完整原生证据 | P0 | 功能已大量实现，但缺维护硬件和真实 Codex 闭环 |
| 统一公开 `nexuscua` CLI | P0 | 公共入口、结构化 `version`、MCP、Codex stdio integration、真人 authority lifecycle，以及 host-bound service/session/app/window/display/clipboard/action/verify 命令族已实现；稳定 exit/error code 与脱敏 stderr 合同已实现；仍缺可信宿主自动启动和完整跨 adapter 撤销 |
| Skill 共同源与双 binding 生成 | P0 | 共同源、canonical catalogue、带 binding digest 的 MCP/CLI 两种单接口 Skill 与漂移检查已实现；CLI 可通过受认证 `session catalogue` 读取服务实时 schema；仍需 clean-host 单 binding 验收 |
| 安装 profile 和 host integration assistant | P0 | unbound/Nexus/MCP 闭合 profile、Codex stdio 的 inspect/plan/apply、单一 MCP Skill、回滚和精确 remove 已实现；仍缺 registered HTTP integration、CLI service 的宿主安装/启动和 clean-host 验收 |
| 渠道证据准入 | P0 | npm platform staging 已要求 accepted manifest 逐文件绑定并语义校验 native/remote/Codex summary；Homebrew/WinGet 生成器会不落盘解包地流式检查有界 tar.gz/ZIP，拒绝路径穿越、链接、特殊文件、重复/冲突层级，要求内外 manifest 精确一致，并复核同一 runtime/profile/证据。仍需 M6 唯一 Codex harness 产出真实 summary 及真实 clean-host 安装验收 |
| CI 工程门禁 | P0 | 所有外部 GitHub Actions 已固定到完整 commit SHA，`workflow-check` 会拒绝浮动/非规范 ref，并已接入 Unix/Windows source gate；仍缺真实 Windows x64 与维护硬件工程门槛 |
| 生产 authority provider handoff | P0 | challenge/grant、OS provider client、Keychain/DPAPI secret、registration MAC、宿主身份复核、launcher、Codex stdio integration、真人 CLI authority lifecycle、CLI service/session open/close，以及 128 registration/512 integration-state 的跨进程锁定硬上限已实现；运行中 provider 会复核 registration 并在临近到期时续租同一 challenge；仍缺可信宿主自动启动和全部运行中 adapter 的完整撤销编排 |
| Nexus 下游 broker/installer | 后续 | 必须等本仓完成，不在当前仓实现 |
| 真实 secure-stream 双端集成 | 已实现（本仓范围） | 两进程临时 mTLS reference harness、断线 reconcile、epoch replay 拒绝、artifact/frame 上限与脱敏证据已实现；生产 listener 仍由后续宿主提供 |
| 可选 VM backend | P1 | provider contract 已有，实际 backend 未实现 |
| 有界录制/轨迹 | P1 | 默认关闭，若实现必须独立授权和限制保留；与 §2.6 的实时指示无关，指示不录制、不保留帧 |
| synthetic per-agent cursor | 已并入 P0 | 作为 §2.6 产品级指示的一部分（Agent 光标/点击涟漪），不再单列 |
| Linux | P1 | 需要完整 X11/Wayland/AT-SPI 与维护矩阵 |

### 8.10 trycua 对照基线

对照快照固定为 `trycua/cua` revision
`605ce1aec80eb73e23ea4c3622b935147a80a572`，观察日期为 2026-09-01。该项目只
作为能力和工程化输入，不是协议兼容承诺；不得复制其 schema、PID/handle authority
或与本项目安全边界冲突的行为。每次准备正式发布前必须先更新固定 revision 和本表。

参考：

- [trycua driver README](https://github.com/trycua/cua/blob/605ce1aec80eb73e23ea4c3622b935147a80a572/libs/cua-driver/README.md)
- [trycua MCP tools](https://github.com/trycua/cua/blob/605ce1aec80eb73e23ea4c3622b935147a80a572/docs/content/docs/reference/cua-driver/mcp-tools.mdx)
- [trycua contracts](https://github.com/trycua/cua/blob/605ce1aec80eb73e23ea4c3622b935147a80a572/docs/content/docs/reference/cua-driver/contracts.mdx)

| 领域 | nexus-cua 当前判断 | 后续动作 |
| --- | --- | --- |
| 应用/窗口发现与截图 | 已实现，并使用更严格的 opaque refs 和代际复核 | 补 Windows x64 与复杂显示拓扑证据 |
| Accessibility 与语义动作 | macOS/Windows 源码已覆盖主要动作 | 补全部维护硬件和 hung-provider 证据 |
| MCP、zoom、normalized coordinates | stdio/Streamable HTTP 共享 server core；真实 rmcp stdio client 与 HTTP JSON/request-scoped SSE initialize/tools-list 已逐项校验完整 schema 一致性并拒绝 legacy GET；stdio timeout/cancel 与两种 HTTP 表示的 deadline/disconnect 已在 post-admission 门闩下证明同 ID reconcile 且原生状态只变化一次；真实 loopback 测试覆盖 connection semaphore、idle 回收、partial authenticated body request deadline 和 shutdown socket 取消；mutation 使用同一有界 ledger；provider grant、registered launcher 与 Codex stdio integration 已绑定 namespace/epoch/TTL/宿主身份及单一 Skill；Codex/macOS 已有 stdio source dogfood | 补 registered HTTP integration 和 Windows/Codex 验收 |
| CLI | host-bound service/session/app/window/display/clipboard/action/verify、同 ID reconcile、实时 schema catalogue 和生成式单接口 CLI Skill 已实现；Agent 路径不接收 transport secret | 补可信宿主自动启动、全部运行中 adapter 撤销编排和 CLI/MCP shared-operation 端到端 conformance |
| 前台输入 | 主要动作已实现 | 补每种动作的 delivery/effect 证据 |
| 后台输入 | macOS background click 有限实现，其余部分完成 | 只扩展公共 API 能准确寻址的类型 |
| 应用/窗口/剪贴板生命周期 | 源码已实现 | 补 Windows 和 clean-host 证据 |
| 多 Agent session | 已实现有界 session 和单桌面 fail-fast lane | 补取消、公平性和资源证据 |
| 多 desktop workers | host registry/lease/epoch 合同已实现 | 接真实远程 host 或 VM pool |
| Remote observation/action | gateway、path-free artifact、两进程临时 mTLS、断线 reconcile 与 `nexus.cua.remote-transport.v1` 证据已实现 | 后续宿主接生产 TLS/SSH/RPC 通道；不在本仓增加公网 listener |
| 网络/文件 policy | descriptor/provider 合同已实现 | 接真正执行策略的 isolated provider |
| VM 与 snapshot/reset/clone | 合同已实现，backend 缺失 | P1 实现一个可选 provider，本地桌面继续 unsupported |
| Recording/trajectory | 核心缺失 | 如需要，在 Runtime 之上做 opt-in 有界 recorder |
| Linux | 缺失 | P1 完整实现后再加入支持矩阵 |
| Browser DOM/CDP | 永久边界 | 保持独立 Browser capability |

## 9. 安全、权限和隐私

### 9.1 权限层次

| 层次 | 发出者 | 范围 | 生命周期 |
| --- | --- | --- | --- |
| 安装许可 | OS 用户/管理员 | 一个精确包版本 | 一次安装事务 |
| 宿主注册 | 宿主用户 | 一个 host、profile、Skill binding | 直到卸载注册 |
| 功能启用 | 产品用户/Owner | 产品级 Computer Use | 直到关闭 |
| Session authority | 可信 Host | 应用、动作、worker epoch、TTL | 短期 session |
| Mutation approval | Host policy 或审批 UI | 单动作或有界动作集合 | request/operation |

任何一层都不能自动推出下一层。

### 9.2 未授权允许范围

未绑定 authority 时只允许：

- 产品、构建和协议版本；
- 静态 schema 与 capability 名称；
- 包签名和完整性状态；
- 不包含应用/窗口/内容的 OS permission status；
- 脱敏的宿主注册状态。

应用、窗口、显示器发现，截图，accessibility，剪贴板和所有 mutation 都需要显式
read 或 mutation grant。

### 9.3 数据生命周期

- screenshot 和 accessibility 内容默认只在内存或私有临时目录短期存在；
- 普通日志禁止记录 screenshot、AX 值、clipboard、typed text、token、grant、
  request payload、native handle 或本地路径；
- artifact 使用字节数、数量和 TTL 上限；
- session 到期、关闭或 runtime shutdown 时自动清理；
- durable audit 只保存非敏感元数据和 action evidence；
- 若 Nexus 以后需要历史或轨迹，必须作为独立、显式授权的持久化服务。

### 9.4 幂等、超时和重放

- 同 request ID、同 command 的并发请求只执行一次；
- 同 request ID 配不同 command 必须失败；
- mutation timeout 后只允许用同 ID 延长等待进行 reconciliation；
- `indeterminate` 不得用新 ID 重试等价动作；
- runtime/worker epoch 变化后旧 mutation 不得跨 epoch 重放；
- ledger 满时拒绝新 mutation，不能驱逐仍在协调窗口内的结果。

Adapter-facing 合同必须让这套规则真正可用：

- 所有 mutating CLI/MCP operation，包括 launch、terminate、clipboard write 和
  action，都必须携带由可信 Host 生成的稳定 `request_id`；
- CLI 使用统一的 `--request-id`，成功、失败和超时结果都回显该 ID；
- MCP 不能把 JSON-RPC request ID 当成 mutation identity；每个 mutation tool 的
  closed input 都有独立 `request_id` 字段；
- CLI 提供 `nexuscua mutation reconcile --request-id <id>`；
- MCP 提供只读 leaf `reconcile_mutation`，受管宿主中的限定名为
  `mcp__nexuscua__reconcile_mutation`；它只查询 ledger，不能重新 dispatch；
- reconcile 结果返回 runtime/worker epoch、admission、disposition、effect evidence
  和是否需要 fresh observation；
- adapter 自动生成 ID 只能用于确认尚未发送的首次调用；一旦可能 admitted，Host
  必须保存并复用返回/预分配的 ID；
- MCP adapter 必须使用与 private transport 相同语义的有界 ledger，或调用同一个
  dispatcher；不能因为嵌入 Runtime 就绕过 reconciliation；
- adapter 进程重启后若旧 ledger 不可证明，返回 epoch changed/indeterminate，
  绝不重新执行动作。

CLI/MCP conformance 必须注入一次 post-admission timeout，证明二者都能以同一
`request_id` 查询结果且 fixture 状态只变化一次。

## 10. Skill 设计

### 10.1 内容结构

共同 Skill 源包含：

- 何时使用 Computer Use；
- observe-before-act；
- 应用和窗口精确选择；
- normalized coordinates 与 zoom；
- 每次 mutation 后重新 observe；
- stale、busy、denied、unsupported、indeterminate 的处理；
- 敏感数据规则；
- verification 和 cleanup。

binding 只包含当前宿主的具体接口调用。

官方 MCP binding 固定绑定 alias `nexuscua`，只引用
`mcp__nexuscua__<leaf>`；不得把内部 Cargo package `nexus-cua-mcp`、handshake identity
或已经删除的 `nexus_cua_*` leaf 当成另一套接口写进 Skill。仓库不生成旧名称
binding，也不维护两份 Skill。

### 10.2 构建要求

- MCP/CLI binding 从同一 action catalogue 生成或校验；
- 每个 binding 只能出现自己的接口；
- 文档中的 operation 必须存在于对应 schema/help；
- 参数范围、默认值、错误和动作效果必须一致；
- 生成结果带产品版本和内容 digest；
- CI 检查生成漂移、重复接口和 schema 总体积。

## 11. CLI 产品设计

### 11.1 当前状态

当前统一的 `nexuscua` CLI 已实现：

```text
nexuscua serve
nexuscua doctor
nexuscua version
nexuscua schema
nexuscua request
nexuscua mutation reconcile
nexuscua mcp launch --registration <non-secret-id>
nexuscua mcp serve --transport stdio|http
nexuscua integration inspect|plan|apply|remove
nexuscua authority plan|apply|status|revoke
nexuscua service start|stop|status --registration <non-secret-id>
nexuscua session open|catalogue|close|status --registration <non-secret-id>
nexuscua app list|list-launchable|launch|terminate --registration <non-secret-id>
nexuscua window list|observe|zoom --registration <non-secret-id>
nexuscua display list|observe --registration <non-secret-id>
nexuscua clipboard read --registration <non-secret-id>
nexuscua action <canonical-mutation-leaf> --registration <non-secret-id> --input-stdin
nexuscua verify --registration <non-secret-id> --input-stdin
```

其中 `serve` 和 `request` 是维护者/宿主诊断入口，不能写入 Agent Skill，也不能成为
生产 Agent 的任意协议逃生口。Agent-facing 命令经精确宿主/profile 认证后连接动态
loopback 内部服务，不接受 endpoint/token；`session catalogue` 从同一服务实时读取完整
tool input schema。`mutation reconcile --registration` 是同 ID 的只读公共协调入口。
CLI 已实现稳定退出码和脱敏错误报告（见下节）；provider 已支持运行中 registration 复核与同 challenge 有界续租；仍需补齐可信宿主自动 service 启动和跨 adapter
主动撤销。

### 11.2 CLI 错误与退出合同

执行成功返回 0。执行失败在 stderr 输出一行 `nexus.cua.cli-error.v1` JSON，
字段为 `schema`、`code`、`exit_code`、`mutation_status` 和 `recovery`。
错误报告只含封闭分类，不回显输入、路径、平台异常或 provider secret。
服务日志也写 stderr，消费者按 `schema` 筛选最终错误报告。
Clap 参数解析错误仍使用标准诊断和退出码 2；`--help`/`--version` 返回 0。

| exit code | code | 含义 |
| --- | --- | --- |
| 1 | internal_error | 启动或内部故障 |
| 2 | invalid_input | CLI 输入或配置错误 |
| 3 | authority_failed | authority/provider/policy 拒绝 |
| 4 | state_conflict | 注册状态漂移、陈旧计划或未完成回滚 |
| 5 | service_unavailable | 已注册服务未运行或宿主不存在 |
| 6 | communication_failed | I/O、响应格式、容量或等待故障 |
| 7 | operation_failed | Runtime/MCP 返回明确失败 |

Runtime/MCP 的完整响应仍写 stdout，包括原始错误 code、request ID 和 disposition；
`isError=true` 或 runtime error outcome 必须返回非零。stderr 不重复响应内容。
`mutation_status=null` 表示此边界没有执行状态证明，不能当作 `not_dispatched`。
对已经提交的 mutation，未知状态或 `indeterminate` 只能 reconcile 原 request ID，
不得根据进程退出码自动重发。成功退出也不替代 action effect 的 fresh verification。

### 11.3 目标状态

其余公共命令继续在 `nexuscua` 下补齐，并遵循 Nexus CLI 风格：

- noun/action 结构；
- `--json` 输出给 Agent 和自动化；
- stdout 只输出结果，stderr 输出诊断；
- 稳定 exit code 和 error code；
- 配置变更采用 inspect/plan/apply；
- Agent 路径不出现隐藏交互 prompt；
- 与 `nexusctl`、`nexuscfg` 无名称和语义冲突。

目标命令族：

```text
nexuscua doctor
nexuscua version
nexuscua schema
nexuscua service start|stop|status
nexuscua mcp launch --registration <non-secret-id>
nexuscua mcp serve --transport stdio|http
nexuscua integration inspect|plan|apply|remove
nexuscua authority plan|apply|status|revoke
nexuscua session open|catalogue|close|status
nexuscua app list|list-launchable|launch|terminate
nexuscua window list|observe|zoom
nexuscua display list|observe
nexuscua clipboard read
nexuscua action <operation>
nexuscua mutation reconcile
nexuscua verify
```

具体参数在 canonical action catalogue 完成后冻结。Agent-facing 命令必须在启动前
绑定 host authority，不能接受模型提供 token、authority path、PID、HWND、应用路径
或权限覆盖参数。

## 12. MCP 产品设计

### 12.1 Transport

MCP transport 是启动配置，不是另一套产品或 tool schema。一个 adapter 进程只选择
一种 transport，两种 transport 必须复用同一个 server implementation、alias、tool
catalogue、authority、request ledger、profile 和错误语义，不能分叉实现。

| 配置值 | 协议含义 | 主要用途 | M3 要求 |
| --- | --- | --- | --- |
| `stdio` | 当前标准 stdio | Codex、本机 Host 按需拉起 | 必须支持，默认值 |
| `http` | 当前标准 Streamable HTTP | 独立常驻 adapter、多个受信客户端、远程 Host gateway | 必须支持 |

这里的 `http` 精确指 MCP Streamable HTTP，不是自定义 REST API。按当前固定的 MCP
2026-07-28 transport revision，请求通过单一 `/mcp` endpoint 的 HTTP POST 发送，响应
可以是 `application/json`，也可以是该请求范围内的 SSE stream；SSE 在这里是 HTTP
响应方式，不是单独的 transport 配置。产品不实现已被 Streamable HTTP 取代的旧
HTTP+SSE wire，也不提供兼容回退。
参考 [MCP Transport specification](https://modelcontextprotocol.io/specification/2026-07-28/basic/transports)
和 [Streamable HTTP](https://modelcontextprotocol.io/specification/2026-07-28/basic/transports/streamable-http)。

目标启动方式：

```text
nexuscua mcp serve --transport stdio
nexuscua mcp serve --transport http --bind 127.0.0.1 --port 0 --endpoint /mcp
```

配置规则：

- 未指定 transport 时使用 `stdio`；不存在 `auto` 猜测和运行中切换；
- `http` 默认只绑定 loopback，默认 `port=0`，由 OS 分配空闲端口，避免固定端口
  冲突；受管 Host 通过 owner-private startup result/endpoint file 获得实际 URL；
- 固定端口只在可信 Host 或用户显式配置时使用；裸安装绝不自动监听；
- `http` 每个连接都必须先完成 transport authentication，再进入 authority
  provider；token、provider grant 和 sidecar credential 不能放进 URL、日志或模型配置；
- HTTP 必须校验 `Origin`，默认无 CORS；非法 Origin 在读取 JSON-RPC body 或解析工具前
  返回 403；
- 非 loopback bind 必须同时显式启用 remote、TLS 和可信认证配置；不能用
  `--bind 0.0.0.0` 单独开启远程桌面控制；
- `http` 按固定规范 revision 实现，不使用已移除的 GET stream 或 MCP 协议级 session；
  Runtime capability session 与 MCP transport connection 始终是两类身份；
- 不提供旧 HTTP+SSE endpoint/message 双路由或兼容回退；
- 两种 transport 都有相同的 frame/body、连接数、in-flight request、HTTP
  request-scoped SSE response buffer、
  keepalive、deadline 和 graceful-shutdown 上限；慢客户端不能造成无界缓存；
- transport disconnect 只取消等待，不能把已 admitted 的 desktop mutation 宣称为
  cancelled；调用方仍须以同一 `request_id` reconcile；
- MCP HTTP 只是 Agent Host 到 adapter 的 transport，不会把 private Runtime
  sidecar socket、worker endpoint 或桌面凭据暴露到网络。

### 12.2 Tool profiles

| Profile | 用途 |
| --- | --- |
| `vision` | Kimi、GLM 和其他视觉 function-calling 模型；flat schema、无复杂 `oneOf` |
| `full` | Codex 验收、能力完整的 Host、调试与 contract 覆盖 |
| Host allowlist | 在任一 profile 上进一步裁剪本次可见 tools |

MCP tool 描述必须短、稳定、拒绝未知字段。`vision` 中常用能力使用独立 flat tools：
observe、zoom、normalized click/drag、type、keys、scroll、semantic invoke/value/range。
所有 mutation flat tool 都包含同一格式的 `request_id`；两个 profile 都必须暴露
只读 reconcile tool，且 Skill 明确要求保留 ID，不得把同一意图换 ID 重试。

所有公开 raw tool name 都是无产品前缀的 snake_case leaf，例如：

```text
get_status
list_apps
list_launchable_apps
launch_application
list_windows
observe_window
zoom
click_normalized
type_text
press_keys
reconcile_mutation
verify_state
terminate_application
```

完整限定名只能由宿主 alias 包装，例如 `mcp__nexuscua__get_status`。MCP server 不得
自行发布 `nexuscua_get_status` 或 `nexus_cua_get_status`。MCP response 中的
`implementation.name` 也不能覆盖或改变宿主注册 alias。

### 12.3 快速接入

裸安装后提供三阶段集成助手：

```text
nexuscua integration inspect
nexuscua integration plan --host codex --profile mcp
nexuscua integration apply --plan <opaque-plan-ref> --confirm
```

- inspect 只读取已知宿主位置，不递归扫描整个用户目录；
- plan 记录 host identity、配置 digest、精确版本、Skill digest 和短期 expiry；
- apply 前重新校验配置 digest；
- Codex/MCP profile 固定计划注册 alias `nexuscua`，冲突时拒绝并给出恢复动作，不自动
  重命名；
- plan 显式记录 `transport=stdio|http` 及 transport-specific 配置；本机宿主优先
  `stdio`，独立服务、远程或多客户端宿主选择 `http`；apply 不静默改 transport，
  也不回退到旧 HTTP+SSE；
- 配置已变化时拒绝 stale plan；
- 写入必须原子化，失败时回滚；
- 备份数量和保留期有界；
- remove 只删除本工具创建的注册，不覆盖用户其他配置；
- 宿主有官方插件/API 时优先使用官方入口。

## 13. 安装与发行渠道

### 13.1 裸安装原则

裸安装只放置 Runtime、CLI、MCP Server、manifest、Skill 源、license、notices 和
SBOM，不自动注册 Codex/Nexus，不自动启动后台服务，不自动获得桌面权限。

### 13.2 平台支持矩阵

| 平台 | 架构 | `0.1.x` 状态 |
| --- | --- | --- |
| macOS 14+ | Apple Silicon | 必须支持并通过维护硬件、签名和 Codex 验收 |
| macOS 14+ | Intel | universal2 构建；无维护 runner 前为 experimental |
| Windows 11 23H2+ | x64 | 必须支持并通过维护硬件、签名和 Codex 验收 |
| Windows 11 | ARM64 | 可编译/工程验证；无维护 runner 前为 experimental |
| Linux | x64/ARM64 | `0.1.x` 不支持，列入后续 P1 |

Hosted macOS/Windows Server CI 只用于编译和 contract；不能改变上表的 native
证据状态。上表是目标矩阵，不是当前支持声明。首个 candidate 只能声明
`platform-validation.md` 中通过的精确 OS builds；要声明“macOS 14+”或
“Windows 11 23H2+”范围，必须同时在最低支持 build 和当时最新稳定 build 上完成
required native/Codex matrix。只有一个 runner/build 时，发布说明必须收窄到该精确
build；不得从单一机器外推整个版本范围。

### 13.3 正式渠道规划

| 平台/用户 | 正式渠道 | 说明 |
| --- | --- | --- |
| macOS | Homebrew | 安装签名并公证的正式 archive |
| Windows | WinGet | ZIP/portable manifest，安装 Authenticode 签名的 `nexuscua.exe` |
| 跨平台开发者 | npm | 一个 meta package 加同版本平台 binary packages |
| 宿主安装器/手工 | 签名 archive | 所有渠道共同消费的 canonical artifact |

npm 是发行包；npx 只是 npm 的一次性执行方式，不作为独立包、独立版本或独立
支持面。正式文档以 `npm install -g nexus-cua@<version>` 为准。npx 即使自然可用，
也不能用于 Nexus 常驻 Runtime、MCP 持久注册或 release acceptance。

npm 的平台包必须在发布时已经包含对应签名二进制。禁止 postinstall 或 Runtime
首次启动时再下载不固定的 executable。

### 13.4 macOS 发行要求

- macOS 14+；
- Apple Silicon 为强制维护路径；
- universal2 archive 可包含 `arm64 + x86_64`；
- Developer ID hardened-runtime 签名；
- Apple notarization Accepted receipt；
- Homebrew formula 固定版本、URL 和 SHA-256；
- archive 内含 binary、README、LICENSE、THIRD_PARTY_NOTICES、SBOM、manifest 和证据；
- 干净机器运行 version、doctor、`stdio/http` MCP transport smoke、install/remove
  smoke；
- Screen Recording、Accessibility、Input Monitoring 等权限由用户/宿主授予，
  安装器不得代替用户绕过。

### 13.5 Windows 发行要求

Windows 必须单独维护发行 manifest 和验收，但不维护另一套 Runtime、Skill 或 CLI。

- Windows 11 23H2+ x64 为强制维护路径；
- canonical artifact 为 `windows-x86_64.zip`；
- `nexuscua.exe` 使用 SHA-256 Authenticode 和 RFC3161 timestamp；
- WinGet 使用 ZIP/PORTABLE manifest，支持精确版本；
- 安装 scope、PATH、upgrade、uninstall 行为明确；
- 用户级状态放在 `%LOCALAPPDATA%` 下并设置 owner-only ACL；
- 私有 transport 使用 owner/SYSTEM-only Named Pipe DACL；
- MCP 默认 stdio；HTTP 仅按显式配置启动用户级 adapter，不安装 Windows Service；
- SmartScreen/Defender、UIA/WGC/SendInput、UAC/protected-target 在干净机器验证；
- WinGet、npm 和 ZIP 解出的 runtime SHA-256 必须指向同一签名 payload；
- 不要求 MSI 或安装 EXE。

## 14. 使用方式

### 14.1 源码开发

前置条件：Rust 1.88+，macOS 或 Windows；原生能力还需要对应 OS 权限。

```bash
make check
make doctor
make mcp-build
```

Windows 不强制安装 GNU Make；使用唯一的 PowerShell source quality gate：

```powershell
./scripts/check-windows.ps1
```

该入口覆盖 Rust fmt/clippy/test、schema drift、生成 Skill、Markdown 链接、release
helper、native harness、Go fmt/vet/test、Python 和 TypeScript，并由 Windows CI 直接
调用。它与 `make check` 使用相同门禁，不承担 Windows 11 维护硬件 native acceptance。

启动开发 sidecar：

```bash
make dev
```

当前 MCP source-build 配置和真实工具序列保留在
[Codex MCP dogfood](codex-mcp.md) 中。该文档是开发 runbook，不等于打包候选验收。

### 14.2 独立 CLI

裸安装后可以直接执行 version、doctor、schema 和 integration inspect。涉及应用、
窗口、截图、AX、剪贴板或动作时，必须先连接可信 authority provider，或由真人启动
一个明确应用/动作/TTL 的本地受限 session。

目标使用流程：

```text
nexuscua doctor --json
nexuscua integration inspect --json
nexuscua session open <经过审批的 scope>
nexuscua app list
nexuscua window list
nexuscua window observe
nexuscua action click_normalized
nexuscua window observe
nexuscua verify
nexuscua session close
```

这是一套协议流程，不代表最终参数已经冻结。

### 14.3 Codex/通用 MCP Host

推荐顺序：

1. 安装固定版本的 `nexus-cua`；
2. 执行 integration inspect/plan/apply 或安装官方插件；
3. 安装 MCP 绑定 Skill；
4. Host 通过认证 provider 启动所选 MCP transport：本机 Codex 默认 `stdio`，独立
   服务、远程或多客户端宿主选择 `http`；
5. MCP 暴露经过 authority 与 allowlist 裁剪的 tools；
6. Agent 执行 observe → action → fresh observe → verify；
7. 任务结束关闭 session 并清理临时 artifact。

### 14.4 Nexus

Nexus 下游接入在独立 branch/worktree 中进行，当前 macOS 开发链路为：

1. Nexus 以 `NEXUS_COMPUTER_USE_ENABLED=true` 和已安装的精确 sidecar 路径启用 Computer Use；
2. Agent 只调用内置 `computer-use` Skill 描述的 round-scoped `nexus.command`；
3. Go service 按需生成 owner-private Unix socket、token 和 artifact root，并监督 sidecar；
4. `computer.inspect` → 应用发现/选择 → fresh observation → 人工审批的 action → verify；
5. sidecar 崩溃、disable、owner switch 或 close target 都失效旧 session、observation、element 和 pending mutation；
6. Nexus 负责审批、receipt、审计、安装、签名、升级、回滚和真实桌面验收。

当前 branch/worktree 已完成 service、command domain、内置 Skill、sidecar supervision、
一次性动作审批和定向测试；签名 payload、clean-host、真实 Mac 权限撤销、Codex acceptance
仍未关闭。

**2026-11 修订后的 Nexus 接入设计**（问题清单见 §23.4，本仓只定义合同，实现在 Nexus）：

1. **目标选择即授权**：`select_target` 弹出一次授权卡片，显示应用身份、动作类别、
   是否允许前台输入、时长；用户确认后才打开 Runtime session。session 只授予确认
   的动作集合，不再默认授予全部 capability（尤其是 `terminate`）。
2. **生命周期跟随回合**：session TTL 与 Nexus round 绑定，Host 在 TTL 到期前续期，
   round 结束、用户停止、功能关闭或 owner 切换立即关闭；不得使用固定 120 秒且不续期。
3. **观察与动作**：模型默认拿到应用场景观察（§2.5）；动作使用 `0..1000` 归一化坐标
   并默认返回动作后观察（§2.7）；暴露恢复最小化、窗口移动/缩放、激活应用、启动
   应用、range 控件、长按等 Runtime 已支持的动作；参数上限与 Runtime 合同一致。
4. **逐次确认仅限高风险**：退出应用、剪贴板写入及宿主策略标记的动作逐次确认；
   确认等待不计入观察有效期，确认后先做轻量复核。
5. **可感知**：Nexus 桌面端订阅 presence 事件，渲染目标高亮、HUD、Agent 光标和
   停止按钮，注册全局停止快捷键；用户接管时暂停前台动作（§2.6）。
6. **打包与系统权限**：sidecar 放在 `Nexus.app` bundle 内，与主程序同一 Team ID
   签名，使屏幕录制与辅助功能权限归属于 Nexus；设置页提供权限检测和跳转引导，
   不自动点击系统授权弹窗。Windows 版由 Nexus 主进程在派发前台动作前授予
   sidecar 前台激活许可（`AllowSetForegroundWindow`）。
7. **合入方式**：集成分支先变基到最新 Nexus `main`，SDK 固定到本仓精确 tag/commit；
   满足条件后由 Nexus 按其流程以功能开关默认关闭合入，内部试用通过 §20 验收后
   再默认开启；本仓阶段不直接修改 Nexus `main`。

## 15. 视觉模型兼容

Runtime 不调用模型 provider。兼容性由外部 harness 完成：

```text
MCP image/tool result -> 视觉模型 -> function call -> MCP tool
```

截至 2026-09-01 的产品假设：

| 模型族 | 处理方式 |
| --- | --- |
| Kimi 视觉模型 | 可直接走 image + tool call harness，优先 `vision` profile |
| GLM 视觉模型 | 可直接走视觉 function call，优先 flat tools |
| DeepSeek 官方文本模型 | 需要 harness 自己接视觉模型或视觉前置层，不能冒充直接看见 PNG |
| DeepSeek 派生/私有视觉模型 | 单独验证 image preprocessing、tool grammar 和多轮结果 |

模型发布能力会变化，正式兼容声明前必须重新检查一手文档并完成真实运行。品牌名或
API 文档本身不能替代端到端证据。

每个兼容声明至少证明：

- 模型收到真实 PNG，而不是路径或文字占位；
- opaque refs 和 normalized coordinates 未被 harness 改写；
- full image 与 zoom crop 均能观察和动作；
- mutation 后重新 observe；
- stale/indeterminate 不盲目重试；
- 日志没有持久化敏感图片、AX、输入或 authority。

参考一手资料：

- [Kimi Chat Completions](https://platform.kimi.ai/docs/api/chat)
- [GLM 视觉模型](https://docs.bigmodel.cn/cn/guide/models/free/glm-4.6v-flash)
- [DeepSeek Tool Calls](https://api-docs.deepseek.com/guides/tool_calls/)
- [DeepSeek Responses API](https://api-docs.deepseek.com/api/create-response/)

## 16. 性能与资源目标

性能属于正确性，不是发布后的优化项。

### 16.1 延迟预算

| 操作 | warm p95 |
| --- | ---: |
| IPC dispatch overhead | `<= 3 ms` |
| 应用/窗口发现 | `<= 50 ms` |
| 1920×1080 窗口捕获 | `<= 100 ms` |
| `<=1000` 节点的 interactive semantic snapshot | `<= 120 ms` |
| pixel + semantic combined observation | `<= 180 ms` |
| semantic action preflight + dispatch | `<= 50 ms` |
| foreground action preflight + dispatch | `<= 60 ms` |
| 应用场景观察（单显示器，含模型尺寸缩放与编码） | `<= 150 ms` |
| 动作 + 动作后观察（不含界面稳定等待） | `<= 220 ms` |
| 动作前低分辨率视觉复核 | `<= 20 ms` |

2026-11 修订说明：历史 4K 诊断中窗口捕获 p95 约 166 ms、前台动作 p95 约 128 ms，
主要来自物理分辨率捕获/编码以及动作前整幅重截图。修订后的预算以"捕获阶段缩放 +
warm stream + 低分辨率复核"为前提；交给模型的单张图片默认不超过 1 MiB 编码字节。

### 16.2 资源预算

- 空闲五分钟平均 CPU `< 0.5%`；
- Runtime + stdio MCP 首次截图前 idle physical footprint 目标 `<= 64 MiB`；
- Runtime + HTTP MCP 首次连接前 idle physical footprint 目标 `<= 72 MiB`；
- 单个 4K capture pipeline 的 GPU/CPU buffers 目标 `< 128 MiB`；
- 默认最多 64 local connections、64 in-flight requests、64 sessions；
- Windows 默认最多 4 个 warm capture pipelines，每个最多保留 2 帧；
- §2.6 的活动捕获流按显示器复用，同时最多 2 条，每条只保留最新 1 帧，帧率按需
  （无观察请求时降到最低档），session 关闭或空闲 30 秒即停止；指示状态不得因
  降频而消失；
- MCP image cache 总量上限 32 MiB；
- 每个 session、tree、frame、artifact、ledger、worker、lease 和 retry 都有显式上限；
- 无请求时禁止 active polling；
- 工程 soak 使用 1 MiB RSS noise floor 检测持续增长；文件和 handle 不允许增长噪声；
- 八小时无单调资源增长仍是正式发布门槛，但不在当前开发验收中执行。

更详细的合同见 [Runtime Contract](specs/runtime-contract-v1.md)。

## 17. CI：每次提交的质量门

### 17.1 本地统一入口

```bash
make check
```

它必须包含：

1. Rust fmt；
2. workspace Clippy，warnings denied；
3. Rust unit/contract/integration tests；
4. committed schema drift；
5. Markdown links；
6. release helper tests；
7. native harness fmt/clippy/tests；
8. Go fmt/vet/tests；
9. Python compile/tests；
10. TypeScript typecheck/tests。

### 17.2 GitHub `CI` workflow

触发条件：

- push 到 `main`；
- push 到 `codex/**`；
- pull request；
- 手工 dispatch。

Jobs：

| Job | Runner | 内容 |
| --- | --- | --- |
| Format and schema | Ubuntu 24.04 | Rust 1.88 fmt、locked metadata、schema、docs、crate package verify |
| Native macOS | macOS 14 hosted | Clippy、workspace tests、Go tests、原生代码编译 |
| Native Windows | Windows Server hosted | 唯一 PowerShell source gate：Rust、schema/Skill/docs、release helper、native harness、Go/Python/TypeScript |
| Python matrix | Ubuntu + Windows | Python 3.10–3.13、wheel build |
| TypeScript | Ubuntu | Node 22、`npm ci`、typecheck、tests |

Hosted CI 只能证明编译和可移植 contract，不能替代真实交互桌面证据。

### 17.3 CI 当前状态与待补齐项

- 已实现：所有第三方 GitHub Actions 固定完整 commit SHA，`workflow-check` 持续防漂移；
- 部分实现：canonical action catalogue 驱动 MCP schema 与 CLI/MCP 两种 Skill，CI
  检查 digest/生成漂移并验证每个 canonical leaf 存在 CLI route；仍缺 CLI/MCP
  shared-operation 的真实 adapter 端到端 conformance；
- 已实现：MCP `stdio/http` transport conformance，验证相同 tool catalogue digest、同一
  request ID/reconcile 语义、启动/关闭、非法消息、超时和断线行为；
- 已实现：HTTP security tests，覆盖动态端口、loopback 默认、Origin/认证拒绝、remote bind
  fail-closed、body/connection/in-flight/SSE buffer 上限和慢客户端；
- 已实现：Streamable HTTP conformance 同时验证 `application/json` 和 request-scoped SSE
  response；拒绝旧 GET stream/HTTP+SSE endpoint，不把 transport 连接当成 Runtime
  capability session；
- 已实现：npm platform packages 的版本、binary digest 与无 lifecycle script 检查；
- 部分实现：Homebrew formula/WinGet manifest 生成与有界 archive 语义检查已纳入 source
  gate；真实 clean-host/channel validate 留到 M6；
- 已实现工程 cross-check：Windows x64 编译/source gate 保持，但不得冒充 native acceptance；
- 部分实现：安装 profile、integration plan stale/rollback 与 Skill 单 binding 测试已覆盖
  Codex stdio；仍缺 registered HTTP、CLI host startup 与 clean-host 验收；
- 部分实现：authority provider handoff、普通 unsigned 文件拒绝、宿主/profile 交叉拒绝和
  CLI service TTL/revoke 清理已有测试；运行中 provider registration 复核与同 challenge
  续租已有测试，仍缺全部运行中 adapter 的主动撤销编排。

## 18. 维护硬件流水线

`.github/workflows/native-hardware.yml` 是手工 workflow，只运行在维护的真实交互
桌面 runner：

- macOS Apple Silicon；
- Windows 11 x64。

基础设施前置责任属于本仓 maintainer/release-engineering owner。进入 M1 原生完成门
之前，必须提交并审阅 `benchmarks/runners/macos-apple-silicon.json` 与
`benchmarks/runners/windows-x64.json`，完成 runner labels、签名身份、fixture 安装
位置、OS permission 预授权和 secret ownership。缺少任一 runner 或 manifest 时
workflow 必须 fail closed；不得跳过平台，也不得用 ARM64、hosted runner 或 VM
代替 Windows x64。首份 baseline 只能由通过审阅的物理 runner evidence 生成。

输入：

- `soak_profile`: diagnostic / idle / engineering / release；
- `evidence_class`: diagnostic / engineering_accepted / release_accepted；
- macOS permission revocation；
- Windows elevated protected-target probe。

每个平台执行：

1. 校验 runner manifest；
2. 安装固定 Rust toolchain；
3. 构建 release Runtime、native harness 和 fixture；
4. 运行应用生命周期、窗口、截图、语义、输入、clipboard、display 等矩阵；
5. 运行 preflight 与 post-dispatch fault；
6. 运行 permission/protected-target；
7. 运行 benchmark；
8. 按 profile 运行 soak；
9. 验证 restart 后旧 session/artifact/ref 失效；
10. 聚合证据并绑定 source revision 与 tested runtime SHA-256；
11. 只上传 summary、签名身份信息和 exact tested runtime；
12. 不上传 token、截图、AX tree、clipboard、输入内容或 transient artifacts。

当前开发阶段使用 engineering profile，不执行八小时 release soak。正式发布时
`release` profile 才是强制项。

当前汇总器已升级到下面的 v2 evidence class，并且 release helper 会拒绝 v1 和
class downgrade；但两台维护硬件尚未提供，尚无真实的 accepted evidence：

| Evidence class | 必需内容 | 接受结果 |
| --- | --- | --- |
| `engineering_accepted` | 完整 native/fault/permission/restart、绝对 benchmark、5 分钟 idle、1 小时 4K active engineering soak | 可进入签名 engineering candidate，不能发布 |
| `release_accepted` | engineering 全部内容 + 8 小时 4K active release soak + release-only operator probes | 可进入 release candidate，仍需人工 publish approval |

新 summary contract 使用 `nexus.cua.native-evidence.v2`，至少记录
`evidence_class`、`required_soak_profile`、每个 required report 的 digest、runner
manifest digest、source revision 和 tested payload digests。汇总器的 `--enforce`
必须按请求 class 校验，不能继续用一个布尔
`accepted_release_evidence` 混淆 engineering 和 release。

## 19. CD：候选产物与发布流水线

### 19.1 当前原则

`.github/workflows/release.yml` 是手工、no-publish workflow。它只有只读仓库权限，
不能创建 Git tag、GitHub Release、npm/Homebrew/WinGet 版本或更新通道。

当前 release workflow 已只接受 v2 `release_accepted` evidence，但仍在 native 测试
之后修改 binary 签名；它不能满足本 PRD 的 engineering candidate，也不能把签名后
的 digest 冒充成已完成完整 native 验证的 payload。M6 必须按下面的目标流水线重构后
才能作为验收。

目标 CD 是三个不可倒置的 no-publish 阶段；可以实现为一个编排 workflow 的三个 job
组，也可以实现为三个以 run ID 串联的手工 workflow：

1. `signed-payload` 输入精确 SemVer、`candidate_kind=engineering|release` 和固定
   source revision，构建并签名/公证所有 payload，输出不可变 artifact run ID、文件
   digests 和签名 identity；
2. `acceptance` 只输入 `signed_payload_run_id` 与期望 digests，把同一 artifact 分发
   到 macOS/Windows 维护硬件，运行 native、remote 和 Codex 验收，输出两平台
   native evidence run IDs、remote evidence run ID 与两平台 Codex evidence run IDs；
3. `assemble` 输入 `signed_payload_run_id`、两平台 native evidence run IDs、remote
   evidence run ID、两平台 Codex evidence run IDs 和 `candidate_kind`，只校验并组装，
   禁止再次签名、strip、patch 或重建 binary。

当前 `.github/workflows/release.yml` 仍是 native-first/sign-after 的 legacy
engineering workflow。虽然它不能发布，但也不能生成 M6 accepted candidate；在上述
编排落地前，其 artifact 必须标记 `unaccepted-legacy`，不得进入 baseline 或下游安装。

### 19.2 证据绑定

流水线必须：

1. 查询 signed-payload、native、remote 与 Codex evidence workflow runs；
2. 要求 workflow、status、conclusion、`head_sha`、candidate kind 和 payload artifact
   run ID 完全匹配；
3. 下载 macOS 与 Windows 的 exact signed-and-tested payloads；
4. 校验 native/remote/Codex evidence contract、classification、runner 状态和空
   issues；
5. 校验 payload SHA-256、签名 identity 和所有 evidence 一致；
6. `scripts/release_candidate.py validate-evidence` 接受
   `--required-class engineering_accepted|release_accepted` 并拒绝降级证据。

目标顺序必须调整为：

```text
固定 commit 构建
  -> 对全部可执行 payload 签名/公证
  -> 在维护硬件上测试签名后的 exact digests
  -> 用同一 payload 跑 Codex acceptance
  -> 只组装、不再修改 binary
  -> SBOM / manifest / provenance / archive
```

这样 native evidence、remote evidence、Codex evidence、manifest 和最终 archive
指向相同签名二进制。
若平台签名身份改变导致权限身份改变，必须在新的签名身份下重新授予测试权限并重跑
完整 required matrix，不能只跑 `version/doctor`。

### 19.3 macOS candidate

1. 从固定 commit 构建 universal `nexuscua` 和 MCP payload；
2. 要求 Developer ID 和 notarization secrets；
3. 对全部 executable 进行 hardened-runtime codesign；
4. `notarytool submit --wait` 并要求 Accepted；
5. 把签名后的 exact digests 送到 Apple Silicon 维护 runner；
6. 在 distribution identity 下完成 required native、permission、MCP 和 Codex 测试；
7. 取回相同签名 payload，只复制到 package tree，不再 codesign 或改写；
8. 运行 version、doctor、`stdio/http` MCP transport、install/remove smoke；
9. 生成 SPDX JSON SBOM、manifest 和 provenance；
10. 生成 `macos-universal.tar.gz` 和 SHA-256；
11. 上传 14 天短期 GitHub Actions artifact。

### 19.4 Windows x64 candidate

1. 从固定 commit 构建 x64 `nexuscua.exe` 和 MCP payload；
2. Azure OIDC 登录 Artifact Signing；
3. 对全部 executable 做 SHA-256 Authenticode + RFC3161 timestamp；
4. `Get-AuthenticodeSignature` 必须为 Valid；
5. 把签名后的 exact digests 送到 Windows 11 x64 维护 runner；
6. 在 distribution identity 下完成 required native、protected-target、MCP 和 Codex 测试；
7. 取回相同签名 payload，只组装、不再修改 executable；
8. 运行 version、doctor、`stdio/http` MCP transport、install/remove smoke；
9. 生成 SPDX JSON SBOM、manifest 和 provenance；
10. 生成 `windows-x86_64.zip` 和 SHA-256；
11. 上传 14 天短期 artifact。

### 19.5 候选包布局、渠道测试和 provenance

每个平台 canonical archive 至少包含：

```text
nexus-cua-v<version>-<platform>/
├── bin/
│   ├── nexuscua[.exe]
├── skills/
│   ├── common/
│   ├── cli/SKILL.md
│   └── mcp/SKILL.md
├── profiles/
│   ├── unbound.json
│   ├── nexus.json
│   └── mcp.json
├── schemas/nexus.cua.v1/
├── README.md
├── PRD.zh-CN.md
├── LICENSE
├── THIRD_PARTY_NOTICES.md
├── sbom.spdx.json
├── manifest.json
├── provenance.intoto.jsonl
├── native-evidence-summary.json
├── remote-transport-summary.json
└── codex-acceptance-summary.json
```

Raw archive 携带两个 Skill binding 供安装器选择，但不会同时注册到一个宿主。
manifest 记录每个文件的 path、size、SHA-256、签名状态、产品/协议版本、source
revision、profile/Skill digest 和 native/remote/Codex evidence digests。

npm 规划为一个 `nexus-cua` meta package，加同版本的 scoped platform packages；
meta package 不包含另一套实现，只选择当前 OS/arch 已随 npm tarball 安装的签名
payload。TypeScript SDK 保持现有 `@nexus-research-lab/nexus-cua` 身份，不能与安装
meta package 混包。npm 名称和 scoped platform names 在 M4 开始前完成 registry
占用检查；冲突必须回到显式命名决策。

不发布情况下的 clean-machine 渠道测试：

- npm：对 meta/platform packages 执行 `npm pack`，从本地 `.tgz` 全局安装并验证
  version、digest、`stdio/http` MCP、integration install/remove；
- Homebrew：生成临时本地 tap/formula，指向候选 archive，执行 install/upgrade/
  uninstall 和 digest 检查；
- WinGet：先 `winget validate`，在隔离 runner 启用 local manifest 后执行
  `winget install --manifest <dir>`、upgrade/uninstall，并恢复 runner 设置；
- archive：解压到空目录，验证 checksum/signature 后完成同样 smoke。

Provenance 使用 in-toto/SLSA 兼容 JSONL/DSSE，绑定 workflow identity、source
repository、commit、builder、inputs 和全部 subject digests。GitHub Actions 生成
attestation 时需要最小化 `id-token: write` 与 `attestations: write`，所有 action
固定完整 commit SHA。即使不发布，provenance 文件也随 14 天候选 artifact 保存。

### 19.6 Engineering candidate 与 Release candidate 分离

现有 workflow 需要改造成两种候选类型：

| 类型 | Soak | 签名/SBOM | 发布权限 | 当前阶段 |
| --- | --- | --- | --- | --- |
| Engineering candidate | engineering | 必须 | 无 | 当前验收目标 |
| Release candidate | 8h release | 必须 | 仍无，等待人工批准 | 延后 |

Engineering candidate 可以完成 Windows x64、macOS 签名、SBOM 和干净机器验证，
但不能被称为正式 release candidate。八小时 soak 和任何 registry/GitHub 发布必须
等用户明确要求。

`candidate_kind=engineering` 必须要求两平台 `engineering_accepted`；
`candidate_kind=release` 必须要求两平台 `release_accepted`。artifact 名称、manifest
`release_state` 和 UI 显示都必须带 candidate kind，不能把前者命名为 release。

### 19.7 未来正式发布顺序

只有 Release candidate 全部通过后，才能新增独立、环境保护的 publish workflow：

1. 人工批准 immutable candidate digest；
2. 创建签名 tag；
3. 创建 GitHub Release；
4. 上传原始 archive、SHA-256、SBOM、manifest、provenance；
5. 发布 npm meta/platform packages；
6. 更新 Homebrew formula；
7. 提交 WinGet manifest；
8. 记录回滚版本和已知限制。

当前仓库不得提前添加能绕过上述顺序的自动 publish job。

## 20. 测试与最终验收

### 20.1 Contract 与 SDK

- 所有 command/result/error 有 schema 与 compatibility fixture；
- unknown field/variant fail closed；
- Go/Python/TypeScript 全部通过同一 fixture corpus；
- live sidecar transport 在 Unix Socket 和 Windows Named Pipe 上通过；
- timeout reconciliation 保持 request identity；
- sensitive values 的 debug/log 输出脱敏。

### 20.2 原生平台

macOS Apple Silicon 和 Windows 11 x64 均必须覆盖：

- running/installed discovery，包括无窗口应用、隐藏应用、其他桌面空间与全屏空间窗口；
- cold launch 与激活应用；
- 应用场景观察：右键菜单、下拉菜单、自动补全、独立对话框可见，未授权应用被遮盖；
- hit-test：点击/滚动落在未授权应用上时 `not_dispatched`；
- exact window/display capture；
- mixed DPI 和多显示器，显示器名称/序号/缩放与截图标签正确；
- 活动 session 期间系统录屏/捕获指示可见，关闭后消失；overlay 不进入截图；
- 紧急停止与用户接管；
- AX/UIA semantic tree 和所有声明动作；
- pixel、keyboard、scroll、drag、hold；
- background/foreground 与 foreground restoration；
- window move/resize/minimize/restore；
- clipboard read/write/restore；
- stale target、app restart、window replacement；
- permission revocation/protected target；
- preflight failure 与 indeterminate dispatch；
- sidecar restart；
- benchmark、memory、handle/file 资源证据。

### 20.3 真实 Codex MCP 最终门槛

必须在 macOS Apple Silicon 和 Windows x64 上分别通过。Codex 只能使用安装后的
MCP 绑定 Skill 和 `mcp__nexuscua__*` 限定工具；raw leaves 必须无产品前缀。不能用
shell、文件系统、Browser 或直接 Runtime request 代替。

M5 必须实现一个可重复的 acceptance harness，目标入口为
`scripts/run_codex_acceptance`（名称可调整，但只保留一个入口）。它负责：

1. 校验 candidate digests 与当前 commit；
2. macOS 将签名 fixture 安装到维护 runner 的固定应用位置，Windows 使用仓库脚本
   安装固定 Start Menu shortcut；
3. 要求 distribution identity 所需 OS permissions 已由 runner operator 预授权；
   缺失时失败，自动化不得点击系统授权 prompt；
4. 从 runner secret/credential store 启动注册 authority provider，在 workspace 外
   创建短期 Codex registration，authority 只允许本 fixture、两次业务 mutation、
   cold launch、observe、verify 和 cooperative terminate；
5. 生成临时 Codex 配置，只启用本候选 stdio MCP、MCP 绑定 Skill 和所需 tools；
6. Codex 外层 write approvals 只预批准固定 fixture lifecycle 和两次动作；
7. 使用提交到仓库的固定 prompt、期望状态和超时，不接受人工改写中途结果；
8. 解析 Codex machine-readable event stream：直接 MCP event 必须是
   `server = "nexuscua"` 且 `tool = <unprefixed-leaf>`；若 Codex 通过编排层调用，
   nested tool identity 必须属于固定 `mcp__nexuscua__<leaf>` allowlist。发现 shell、
   filesystem、Browser、network、其他 MCP alias 或带旧 `nexus_cua_` 前缀的 leaf
   立即失败；
9. 清理 fixture、Codex registration、provider grant、临时配置和 artifact；
10. 输出 `nexus.cua.codex-acceptance.v1` summary。

summary 至少包含 source revision、candidate/file digests、OS build、runner manifest
digest、Codex version、模型 provider、精确 model ID/可用 snapshot、reasoning 配置、
固定 prompt digest、tool schema digest、Skill digest、MCP profile、provider identity
digest、授权 scope digest、允许/实际 tool sequence、每步 request ID、attempt index、
状态断言、timings、cleanup 结果和所有 policy violations。它不包含图片、AX、
clipboard、输入文本、token 或 grant。

每个平台对固定 candidate 连续执行 3 次业务流程，必须 3/3 通过；不得挑选成功结果。
只有 runner 离线、Codex service unavailable 等在步骤 2 前发生、且未执行任何 tool 的
基础设施错误可重跑，最多 2 次，所有无效 attempt 仍写入 summary。任何 tool sequence、
状态断言、reconcile 或 cleanup 失败都是候选失败，不能用重跑覆盖。

确定性流程：

1. 初始没有 fixture process；
2. Codex 读取 status 和经过 authority 裁剪的能力；
3. 发现唯一允许的已安装 fixture；
4. cold launch，不提供路径、PID 或 native handle；
5. 选择精确窗口；
6. observe，真实图片显示 `counter=0` 和 `checked=false`；
7. 使用 fresh observation 执行一次 normalized click，验证 `counter=1`；
8. 重新 observe；
9. 使用新 observation 执行一次 semantic toggle，验证 `checked=true`；
10. 再次 observe 并确认两个状态；
11. 协作退出精确进程代际；
12. 确认应用列表和窗口列表均为空。

另一个固定 fault 场景在 post-admission 注入 timeout：Codex 必须使用
`mcp__nexuscua__reconcile_mutation` 查询相同 `request_id`，fixture 状态只改变一次；
若 Codex 重新提交新 ID、adapter 再次 dispatch 或无法证明 disposition，则整个平台
验收失败。

证据记录 source revision、OS build、Codex version、package digest、Skill digest、
MCP profile、tool-call events、timings 和脱敏结果。任一平台缺失只能算平台局部通过。

### 20.4 模型兼容验收

Kimi、GLM 或 DeepSeek 组合只有在具备相应 API/模型凭据时执行真实 harness 测试。
没有凭据时不阻塞 Runtime/MCP contract，但不能发表对应模型兼容声明。

## 21. 开发顺序

### 2026-11 优先级调整

核心观察/动作循环尚未在 Nexus 中跑通真实任务，而仓库已同时推进 remote gateway、
worker registry、VM 环境合同、三套 SDK、多渠道打包和 authority provider。修订后：

- **先做 R0 → R1 → R2**，它们是 Nexus 可用的前提；
- M1（Windows）与 R1 并行，但 Windows 实现直接按修订后的合同做，不再补旧设计证据；
- **冻结** M2 中 remote/VM/policy 的新增工作（保留现有代码与回归测试，不再扩展）、
  M4 中 Homebrew/WinGet/npm 渠道与 registered HTTP integration、M6 的渠道 clean-host；
  解冻条件是 R2 在 Nexus 内部试用通过；
- M5 的性能证据在 R1 完成后按 §16 新预算重新采集，旧 4K 诊断只作对比。

### R0 — 输入与配置缺陷修复（本仓）

- 修复 §23.3 B1–B8：滚动坐标与 hit-test、滚动单位、键名语义与别名、标点键与键盘
  布局、Windows 扩展键标记、点击/输入节奏、输入法检测、`serve` 观察有效期可配置；
- 为每项增加确定性测试与 fixture 场景。

完成条件：两平台同一动作序列在 fixture 上产生相同效果。

### R1 — 观察模型与可感知（本仓）

- 应用/窗口/显示器发现修正（§23.2 D2、D3）；
- 应用场景观察与 hit-test 授权（§2.5，§23.2 D1）；
- 捕获阶段缩放、低分辨率复核、warm stream、`observe_after`、协议级归一化坐标（§2.7，§23.5）；
- 系统级指示、presence 事件、紧急停止与用户接管合同；独立 MCP 路径的最小指示（§2.6）；
- 同步修改 Architecture、Protocol、Runtime Contract、Skill 与 SDK。

完成条件：§20.2 新增场景在 macOS 通过；Codex MCP 能完成"打开下拉菜单并选择一项"的
真实任务；活动期间系统指示可见。

### R2 — Nexus 接入（Nexus 仓，独立分支）

- 按 §14.4 修订设计实施，关闭 §23.4 N1–N7；
- 真实 Nexus Agent 完成至少三类任务（菜单操作、多窗口、跨显示器）并连续 3/3 通过。

完成条件：集成分支具备以功能开关默认关闭合入 Nexus 主线的条件，可开始内部试用；
是否及何时合入由 Nexus 仓库按其流程决定，本阶段不直接修改 Nexus `main`。

### M0 — 文档和合同收敛

- 本 PRD 成为唯一产品开发入口；
- 删除重复 PRD、roadmap、旧 product integration、旧 trycua/model/release 总结；
- 修复 README 和保留文档的引用；
- 冻结产品边界、profile、Skill binding 和验收矩阵。
- 指定 macOS/Windows 维护 runner 与 release-engineering owner；未具备的 runner 作为
  显式基础设施 blocker 记录，不能降级矩阵。

完成条件：仓库不存在互相冲突的产品路线文档。

### M1 — 原生功能和 Windows x64

- 完成所有已声明 action 的 macOS/Windows fixture 覆盖；
- 完成 Windows x64 discovery、launch、capture、AX/UIA、input、clipboard、window；
- 完成 background 和 foreground restoration 证据；
- 修复跨平台错误和资源泄漏。

完成条件：两平台维护硬件 native matrix 通过。

### M2 — Worker、remote、policy 和环境生命周期

- 完成多 session/worker 并发和 epoch/lease 行为；
- 完成 secure-stream 双端 reference mTLS 集成测试和
  `nexus.cua.remote-transport.v1` 证据；生产 listener 不在本仓；
- 完成网络/文件 policy descriptor 与 isolated test provider；
- 完成 snapshot/reset/clone contract 与 bounded test provider；
- 本地桌面保持 lightweight 和 unsupported lifecycle。

完成条件：动作不能跨 worker/epoch 漂移或重放。

### M3 — CLI、MCP 和 Skill

- 统一 `nexuscua` 公共 CLI；
- 统一 MCP alias/handshake identity 为 `nexuscua`，把 raw tools 从旧
  `nexus_cua_*` 直接重命名为无前缀 leaf，并删除旧名称，不提供兼容 alias；
- 实现单一 server core 的 `stdio/http` 可配置 transport，默认 stdio；
- 保持 MCP `vision/full`；
- 建立 canonical action catalogue；
- 生成 CLI/MCP 两种 Skill binding；
- 完成 authority provider handoff；
- 为全部 mutation 增加 adapter-facing request ID 和只读 reconcile；
- 验证 shared operations 语义一致。

完成条件：每个宿主只能看到一种接口，两个 adapter 不分叉。

### M4 — 安装与宿主接入

- 实现 unbound/nexus/mcp profiles；
- 实现 integration inspect/plan/apply/remove；
- 实现 npm meta/platform packages；
- 实现 Homebrew 和 WinGet manifests；
- 实现 Codex 快速注册或 plugin bundle；
- 只定义 Nexus installer contract，不修改 Nexus 仓库。

完成条件：裸安装不改宿主；Codex 只激活 MCP binding；Nexus fixture 只选择 CLI binding。

### M5 — 性能与工程化

- 完成 CI 缺口；
- 实现 `native-evidence.v2` 与 engineering/release 两种 class；
- 跑 benchmark、memory、fault、restart、permission 和 engineering soak；
- 控制 schema/context 体积；
- 验证 install/repair/upgrade/rollback/uninstall；
- 检查空闲 CPU、RSS、frame/artifact/ledger/worker 上限。

完成条件：`make check` 和两平台非发布工程门槛通过。

### M6 — 打包与 Codex 验收，不发布

- 先签名 macOS 与 Windows x64 payload，再用签名后的 exact digests 完成 native/
  Codex 验收并组装 engineering candidates；
- 生成 SBOM、checksum、manifest、provenance 和 notices；
- 干净机器验证 Homebrew/WinGet/npm/archive；
- 两平台真实 Codex MCP 最终流程通过；
- 不执行八小时 soak；
- 不创建 tag、GitHub Release 或 registry 版本。

完成条件：本仓 `nexus-cua` 可以独立交付给下游 Nexus 集成。

### M7 — Nexus 下游集成（已开始，独立 branch/worktree）

M7 已基于最新 Nexus `main` 建立独立开发 branch/worktree。当前 macOS 批次已实现：

- package path 配置、pinned Go SDK client 和 sidecar supervisor；
- `computer` domain 的 inspect、contract、应用发现、目标选择、observe、action、verify、close；
- 内置 `computer-use` Skill、round/owner identity、一次性真人审批和敏感输入脱敏；
- sidecar crash/restart、disable、owner switch、round cleanup 与 request reconcile 测试。

仍需关闭：

- UI/`nexuscfg` 安装与精确 payload pinning；
- macOS 稳定签名身份、真实权限撤销、mixed-DPI/background/restoration；
- 真实 Codex acceptance、clean-host 安装生命周期和升级/回滚；
- 与 Nexus Agent 真实 runtime 的 inspect → action approval → verify 闭环。

M7 不替代本仓 M0–M6 的签名、双平台和发布门槛。

## 22. Definition of Done

本仓完成必须同时满足：

1. 一份 canonical action catalogue 驱动或校验 protocol、SDK、CLI、MCP 和 Skill；
2. macOS Apple Silicon 与 Windows x64 原生矩阵通过；
3. 所有桌面读写均受 authenticated authority 控制；
4. unbound/Nexus/MCP profile 行为明确且可测试；
5. 单宿主 Skill 不包含双接口；
6. CLI 与 MCP 共同能力语义一致；MCP `stdio/http` 具有相同 catalogue、authority
   和有界资源行为，所有 mutation 有稳定 `request_id` 和只读 reconcile；
7. background、多 session、多 worker、remote 和 policy 行为有界；
8. snapshot/reset/clone contract 完整，本地桌面明确 unsupported；
9. 性能、RSS、CPU、队列、缓存和短期 engineering soak 达标，并由
   `native-evidence.v2` engineering class 证明；
10. macOS 与 Windows x64 engineering candidates 已签名，native/Codex 验收绑定
    签名后未再修改的 exact payload digests；
11. SBOM、checksum、manifest、provenance、notices 和干净机器 smoke 完整；
12. 真实 Codex MCP 在两个平台通过最终流程；
13. 没有发布任何正式版本；
14. 八小时 soak 被明确保留为未来 release gate；
15. Nexus `main` 主分支仍未被本阶段直接修改；当前集成只存在于独立 branch/worktree；
16. 默认观察是应用场景，菜单/弹窗/多窗口可见，未授权应用像素不可见，像素动作
    经 hit-test 授权（§2.5）；
17. 活动 session 期间系统级指示持续可见，presence 事件、紧急停止和用户接管可测试（§2.6）；
18. 所有入口使用协议级归一化坐标，动作可返回动作后观察，交给模型的图片符合
    图像预算（§2.7）；
19. §23 列出的 D/B/P/N 类问题全部关闭或有明确的延期记录。

## 23. 2026-11-02 设计与实现审查

本节取代 2026-10-26 的执行盘点，是本 PRD 当前的执行快照，不新增另一份路线图。
问题按类别编号：D = 设计层，B = 实现缺陷，P = 性能，N = Nexus 集成，
W = Windows 测试重点。每个问题都给出现象、根因（含源码位置）、解决方案和验收条件；
关闭一个问题必须同时满足其验收条件并在本节更新状态。

### 23.1 审查范围与总体结论

- `nexus-cua`：本地 `main`，提交 `e5944fc`；`cargo test --workspace` 通过。
- Nexus：Computer Use 集成位于独立 `cua` 分支（worktree
  `worktrees/nexus/cua-macos-integration`，提交 `2ffa5ee6a`），该分支落后 Nexus
  `main` 106 个提交；功能开关 `NEXUS_COMPUTER_USE_ENABLED` 默认关闭。
- 方法：阅读当前源码、PRD、平台验证记录和本地 `target/native-evidence` 历史报告，
  结合用户真实使用反馈。未重跑真实桌面动作，未验证签名候选或干净机器安装。

结论：

1. **Runtime 的安全与可靠性内核是对的，保留**：opaque refs、进程/窗口代际复核、
   新鲜观察、同 `request_id` 对账、有界资源、平台 actor 所有权。
2. **产品层的观察模型和用户可感知设计是错的，必须修订**：只观察单个窗口、
   一次性截图、无任何操作反馈，直接造成"看不到 app、不知道 Agent 在干什么、
   分不清屏幕"。修订方向见 §2.5–§2.7，具体问题见 §23.2。
3. **存在会让两平台行为不一致或越过授权边界的输入缺陷**（§23.3）。
4. **Nexus 集成分支有几处参数和流程设计会让真实任务必然失败**（§23.4）。
5. **范围超前于产品验证**：核心循环尚未在 Nexus 中跑通，仓库已同时推进 remote、
   VM、多渠道等工作；按 §21 冻结非关键工作。

| 里程碑 | 当前判断 | 修订后处理 |
| --- | --- | --- |
| R0 | 新增；输入与配置缺陷未修 | 立即开始 |
| R1 | 新增；场景观察、可感知、交互循环均未实现 | R0 后开始，与 M1 并行 |
| R2 | Nexus 分支有基础实现，但存在 N1–N7 | R1 合同稳定后实施 |
| M0 | 主 PRD 已收敛，本次修订后需同步技术文档 | 随 R1 实施同步 |
| M1 | macOS 驱动与 fixture 已存在；历史 native 报告 15 个场景组通过（旧设计） | Windows 按修订合同实现；macOS 补 §20.2 新场景 |
| M2 | host/registry/gateway/environment 与两进程 mTLS harness 已存在 | 冻结新增，只保留回归 |
| M3 | CLI、双 MCP transport、Skill、provider 主体存在 | 剩余项排在 R2 之后 |
| M4 | profile、Codex stdio assistant、渠道生成器存在 | 渠道与 registered HTTP 冻结 |
| M5 | source gate、harness、Codex acceptance 入口存在 | R1 后按新预算重新采集 |
| M6 | checksum/SBOM/manifest、签名预检存在 | R2 内部试用通过后再推进 |

### 2026-11-02 实施复核（进行中）

- 用户确认既有发布只用于测试流水线，无实际用户，本轮不保留旧协议兼容层。
  Runtime、SDK、Nexus 必须一起重建；坐标合同改为统一 `NormalizedPoint`，滚动点必填。
- 已修改：B1 显式滚动点与派发 hit-test、B2 系统滚轮设置换算、B3 删除方向、
  B4 `mod` 与常用别名、B6 扩展键/扫描码、B7 指针预移动及有界 Unicode 分块节流、
  B8 统一有效期与服务参数。B4 capabilities 已报告主修饰键，B5 已使用当前布局解析
  字符与符号；无修饰的单字符按键走 Unicode 输入。B7 输入法组字状态的完整检测
  仍待完成，不能把源码修复标成整项原生验收通过。
- B2 验收需区分逻辑意图和应用滚轮量化：Windows 按系统行/字符数与每单位 16
  逻辑像素换算，禁用/整页滚动配置明确拒绝。跨任意应用的像素误差 ≤10% 不能仅
  从 SendInput 返回值推导，须在指定 fixture 中测量。
- Nexus `cua` 已合并本地 `main` `74d8fe98b`，合并提交 `2b2d5d031`；旧沙箱
  恢复实现没有复活。合并后的 `make check` 与前端 `npm run check` 已通过；后续
  坐标接入改动单独验证。
- R1 已加入会话绑定的 macOS `SCStream` 窗口捕获，最多两个热流、每流一张
  最新 Rust 帧、10fps、30 秒空闲回收；会话关闭/过期撤销捕获所有权。
  这还不是 PRD 的按显示器共享场景流。系统录屏指示、原生释放时延与性能预算
  尚未验收；presence、用户接管、动作后观察仍待实现。
- 本轮 macOS `make check` 通过（含会话关闭/过期/关闭 runtime 的所有权测试）。
  已定位并修复 fixture 状态标签撑宽窗口导致的 geometry stale；滚动目标改用
  AX hit-test，最小化改读 AXMinimized。持续流版本已通过核心动作、跨屏、窗口替换和会话过期路径；
  修复了已销毁 AX 对象导致列表整体失败的问题。完整回归仍阻塞于最小化状态
  检查。独立 fixture 在无 runtime/捕获时也未成功最小化，尚未确定 AppKit/桌面
  原因。已加入不申请前台输入的 `desktop-conditions` 诊断；临时截图和令牌已清理。
- Windows x64 SSH 测试机已连通，WPF fixture 构建通过；Rust 1.88 GNU 全工作区
  Clippy 及 Python、Go、TypeScript SDK 测试曾通过。LLVM-MinGW 的 `ld.lld`
  配合 `link-self-contained=yes` 已解决原先测试二进制启动与 proc-macro 链接问题。
  随后工作区测试暴露远程 harness 在断线后过早查询的竞态；已加入有界只读
  reconciliation 等待，本地与 Windows 工作区测试均通过。完整原生验收仍未完成。
- Windows 活动控制台 session 2 的原生回归失败在 `observe_display`：WGC
  `CreateForMonitor` 返回 `0x80070057`，尚未进入窗口动作验收。两次临时任务和
  进程均已结束，截图与令牌已删除。窗口单独诊断已进入辅助功能检查，但 fixture
  按钮未暴露预期 invoke，仍未通过。恢复 CI 不代表本轮 Windows 原生测试通过。

### 23.2 设计层问题（D 类）

#### D1 观察只有单个窗口，菜单、弹窗和上下文全部不可见

- **现象**：模型点开下拉菜单、右键菜单或自动补全后重新观察，看不到弹出内容；
  多窗口应用需要逐个切换窗口；模型不知道目标窗口在屏幕上的位置和周边。
- **根因**：窗口截图使用 desktop-independent-window 过滤器，只渲染一个窗口
  （[capture.rs](../crates/platform/src/macos/capture.rs) `load_capture_filter`）；
  菜单、popover、tooltip 在系统中是独立窗口且通常不在 layer 0，而窗口枚举只收
  layer 0（同文件 `native_window`）；整屏显示器观察被设计为另行授权的只读概览，
  不能驱动动作。这是 PRD 早期"观察单位 = 授权单位 = 精确窗口"的直接后果。
- **方案**（§2.5）：
  1. 新增应用场景观察：macOS 使用 ScreenCaptureKit 的
     display + including-applications 过滤器，只包含授权应用及允许的辅助进程，
     天然包含该应用的菜单与弹窗；Windows 使用 WGC 显示器捕获，按 z-order 计算
     授权进程全部顶层窗口（含 popup/菜单类窗口）的可见区域，其余像素遮盖。
  2. 辅助进程允许清单由 Runtime 维护且封闭：例如 macOS 打开/保存面板服务进程可
     归属于发起应用；系统认证、TCC 授权弹窗、登录窗口永远不纳入。
  3. 像素动作派发前 hit-test：macOS 用当前屏幕窗口列表按 z-order 求点所在的
     最上层窗口及其进程；Windows 用 `WindowFromPoint` + 根窗口所属进程。命中
     未授权进程时返回 `target_not_authorized` 且 `not_dispatched`。
  4. 观察结果附带授权窗口清单（ref、标题、在图中的归一化矩形、状态）和显示器标签。
- **验收**：fixture 中打开右键菜单、下拉菜单、自动补全和独立对话框后均可见并可点；
  同屏另一未授权应用在截图中被遮盖；点击/滚动坐标落在未授权应用上被拒绝且无副作用。

#### D2 应用和窗口发现不完整，"经常看不到 app"

- **现象**：已运行但没有打开窗口的应用（关闭了最后一个窗口的浏览器、菜单栏应用）
  不在应用列表；在其他桌面空间、全屏空间或被隐藏（⌘H）的应用窗口被当作"已最小化"，
  观察直接失败；"恢复最小化"对这些窗口无效。
- **根因**：
  - 运行中应用从可见窗口反推（[macos/mod.rs](../crates/platform/src/macos/mod.rs)
    `list_applications` 调用 `windows()` 再去重），没有窗口就没有应用；
  - `minimized` 被定义为 `!visible`（同文件 `driver_window`），把其他空间、全屏、
    隐藏、离屏统统当作最小化；
  - 观察对 `!visible` 直接返回"minimized target cannot provide a frame"；
  - 捕获过滤器只查询 on-screen 窗口，其他空间的窗口必然 `target_unavailable`；
  - `list_windows` 丢弃不可见且无标题的窗口。
- **方案**：
  1. 应用列表以系统运行应用为来源（macOS `NSWorkspace.runningApplications`，
     默认 regular 激活策略，accessory 由宿主策略决定；Windows 以拥有顶层窗口或
     任务栏条目的进程为准），返回窗口数、是否隐藏、是否前台；
  2. 窗口状态改为枚举 `normal/minimized/app_hidden/other_space/fullscreen_space/offscreen`，
     最小化来自 AX `AXMinimized` / Win32 `IsIconic`，其他虚拟桌面来自
     `DWMWA_CLOAKED`；同时返回窗口角色、z-order、所在显示器；
  3. 新增 `activate_application` 动作（需前台授权）：取消隐藏并激活，系统会切换到
     该应用所在空间；macOS 公共 API 不能跨空间移动窗口，不使用私有 API；
  4. 错误携带明确恢复建议：`restore_window`、`activate_application`、
     `open_window`（通过快捷键或菜单），模型不再盲目重试观察。
- **验收**：fixture 覆盖无窗口应用、⌘H 隐藏、其他桌面空间、全屏空间、真正最小化
  五种情况，状态报告正确，按建议动作恢复后可观察。

#### D3 显示器无法区分

- **现象**：多显示器时模型和用户都说不清"哪块屏"；显示器截图与窗口无法对应。
- **根因**：`DisplaySummary` 只有 ref、逻辑坐标和是否主屏
  （[observation.rs](../crates/protocol/src/observation.rs)）；`WindowSummary`
  不含所在显示器；截图没有任何标签。
- **方案**：显示器增加名称（macOS `NSScreen.localizedName` 按显示器 ID 匹配；
  Windows `DisplayConfigGetDeviceInfo` 目标名）、稳定序号（按左→右、上→下）、
  缩放比例、像素尺寸、是否含光标/前台窗口、所含窗口 ref；窗口增加 `display_ref`；
  场景观察的文字结果包含"显示器 2「名称」2560×1440 @1x，位于主屏右侧"一类标签，
  多显示器时附一行布局说明。显示器名称属于可见元数据，仍需要 read authority。
- **验收**：双屏、混合 DPI、负坐标布局下名称、序号、缩放和窗口归属正确；
  模型在固定 prompt 中能按"左边那块屏上的 X 应用"正确选择目标。

#### D4 用户无法感知 Agent 在观察和操作

- **现象**：用户实测 Codex Computer Use 会触发系统屏幕录制指示，本项目没有任何
  指示；Agent 点击前用户不知道它要点哪里，也没有一键停止。
- **根因**：macOS 捕获走一次性截图路线（[Architecture](architecture.md) 明确记载
  "public one-shot screenshot route"），系统不呈现持续录屏状态；协议没有 presence
  事件；PRD 把 Agent 光标列为 P2、录制列为可选，没有把"可感知、可打断"作为
  真实桌面产品的前提。
- **方案**（§2.6）：
  1. 活动 session 期间按显示器保持有界 `SCStream`/WGC 会话，产生系统指示；同时作为
     warm pipeline 取代每次捕获的内容枚举，资源上限见 §16.2；
  2. 协议新增只读 presence 事件订阅：`session_active`、`observing`、
     `action_pending`（动作类型、归一化点、目标窗口 ref）、`action_finished`、
     `session_closed`；事件不含像素、文本或可访问性内容；
  3. 宿主渲染 overlay 并从捕获中排除；`nexuscua` 独立路径提供最小指示；
  4. 紧急停止：宿主快捷键/按钮调用 Runtime 的撤销入口，立即关闭 admission 并关闭
     session；
  5. 用户接管：检测真实用户输入时前台动作返回 `user_active`；实现必须能区分本进程
     合成的事件（macOS 使用自有 event source 标记与时间窗，Windows 使用注入标志），
     优先不需要额外系统权限的方案，若需要必须在权限引导中说明。
- **验收**：活动期间系统指示持续可见、关闭后 5 秒内消失；overlay 不出现在截图中；
  停止快捷键在动作序列中途生效且无后续派发；用户移动鼠标后前台动作被拒绝。

#### D5 交互循环过重，stale 循环频繁

- **现象**：每步至少"观察 + 动作"两次往返；推理模型思考稍久或人工审批稍慢，
  动作就因 stale 失败，模型反复重新观察。
- **根因**：每个 observation 只能驱动一个动作，动作后立即失效
  （[service.rs](../crates/runtime/src/service.rs) `perform_action`）；动作前整幅
  重截图做视觉复核（[macos/mod.rs](../crates/platform/src/macos/mod.rs)
  `observation_is_current`）；`serve` 路径观察有效期固定 30 秒（见 B8）；Nexus 在
  观察之后、派发之前插入人工审批（见 N3）。
- **方案**（§2.7）：动作默认返回动作后观察（有界稳定等待：视觉签名连续两帧不变或
  到上限）；低分辨率复核；观察有效期统一可配；审批等待不计入有效期，复核失败时
  在错误中附带新观察。保持"一个 observation 只驱动一个 mutation"的安全不变量，
  但让下一次 observation 随动作结果免费返回。
- **验收**：固定任务的往返次数下降约一半；120 秒内的审批等待不导致 stale；
  复核失败时模型无需额外调用即可拿到新画面。

#### D6 授权粒度与审批位置

- **现象**：Nexus 每个动作都弹审批；选择目标时却授予了全部 capability。
- **根因**：§9.1 的 "Mutation approval" 层没有区分常规动作与高风险动作，宿主只能
  选择"逐次全审"或"不审"。
- **方案**：Session authority 在目标选择时一次性确认（应用、动作类别、前台输入、
  时长）；Mutation approval 只用于高风险类别（退出应用、剪贴板写入、宿主策略
  标记）；§9.1 的层次不变，但 Host 必须按此分级实现。
- **验收**：常规点击/输入不再逐次弹窗；未确认的动作类别被 Runtime 拒绝。

### 23.3 实现缺陷（B 类，R0 修复）

| 编号 | 问题 | 位置 | 后果 | 修复与验收 |
| --- | --- | --- | --- | --- |
| B1 | 滚动不带坐标，在当前光标下投递；`element_ref` 只做 focus，不改变滚轮目标 | [macOS input](../crates/platform/src/macos/input.rs) `scroll`；[Windows input](../crates/platform/src/windows/input.rs) `scroll` | 光标不在目标上时滚动其他应用，越过授权边界 | 协议 `Scroll` 增加必填点（归一化），派发前 hit-test，事件位置设为该点；fixture 验证光标位于其他应用时仍只滚目标 |
| B2 | 滚动单位不一致：协议为像素，Windows 乘以 `WHEEL_DELTA=120` | Windows input `wheel_delta` | 同样 300，Windows 滚约 300 格 | 统一逻辑像素，Windows 按系统滚动行数换算为 wheel delta；两平台 fixture 滚动距离误差 ≤10% |
| B3 | `delete` 在 macOS 是退格、在 Windows 是向前删除 | 两平台 `key_code` | 同一指令两平台删除方向相反 | `backspace`=退格、`delete`=向前删除（macOS 用 forward-delete 键码）；fixture 断言文本结果一致 |
| B4 | `meta` 在 Windows 映射为 Win 键；不接受 `ctrl`/`cmd` 等常见别名 | Windows input `press_keys`；[validation.rs](../crates/runtime/src/validation.rs) `is_supported_key` | `meta+c` 在 Windows 触发 Win+C；模型常用键名被拒 | 引入平台中立 `mod`，显式 `cmd`/`ctrl`/`win`，入口归一化别名；capabilities 报告平台主修饰键 |
| B5 | 键表缺标点与符号键；macOS 使用 ANSI 物理键码 | 两平台 `key_code`、`is_supported_key` | `cmd+,`、`cmd+=`、`cmd+/` 等常用快捷键不可用；非美式布局按错键 | 增加符号键；macOS 按当前输入源把字符映射到键码，Windows 用 `VkKeyScanExW`；fixture 覆盖美式与一个非美式布局 |
| B6 | Windows `SendInput` 未对方向键、Home/End、PageUp/Down、Delete 设置扩展键标记 | Windows input `keyboard_input` | NumLock 打开时部分应用收到小键盘数字 | 按键码设置 `KEYEVENTF_EXTENDEDKEY` 并填扫描码；NumLock 开/关两种 fixture |
| B7 | 点击前不移动指针、按下/抬起与多击之间无间隔；文字每 20 个 UTF-16 单元连续发送无节流；未处理输入法组字状态 | 两平台 input | 悬停依赖的 UI 不响应、双击不稳、Electron 类应用丢字；中文输入法下 `press_keys` 字母进入组字 | 点击前 move + 平台间隔；文字分块节流；派发前检测输入法组字状态并报告或切换到 Unicode 路径；Electron fixture 输入 2 KB 文本无丢失 |
| B8 | `nexuscua serve` 观察有效期固定 30 秒且无参数，MCP 默认 120 秒 | [commands.rs](../crates/cli/src/commands.rs) `serve` 使用 `RuntimeConfig::new` 默认值 | Nexus 路径 stale 率远高于 MCP，两个入口语义不一致 | `serve` 增加与 MCP 相同的参数和默认值，Runtime 默认值统一；contract test 断言两入口一致 |

### 23.4 Nexus 集成问题（N 类，R2 修复，代码在 Nexus 仓）

| 编号 | 问题 | 位置（Nexus `cua` 分支） | 后果 | 修复与验收 |
| --- | --- | --- | --- | --- |
| N1 | session TTL 固定 120 秒且不续期 | `internal/service/computeruse/service.go` `defaultSessionTTL` | 超过 2 分钟的任务中途 `session_unavailable` | TTL 跟随 round，到期前续期；10 分钟任务不中断 |
| N2 | `click_point`/`drag` 使用截图像素坐标 | 同文件 `buildAction`；`internal/mcp/command/computer/registry.go` schema | Retina 截图经模型侧缩放后点击偏移 | 改为 `0..1000` 归一化（R1 后由协议原生支持）；4K/Retina 下点击误差 ≤ 1% |
| N3 | 人工审批位于观察之后、派发之前 | `performAction` 中 `approve()` | 审批超过有效期必然 stale | 按 D5/D6：常规动作不逐次审批；逐次审批不计入有效期，审批后复核 |
| N4 | `SelectTarget` 授予全部 capability 与前台输入 | `SelectTarget` 中 `AllowedActions: capabilities.Actions` | 授权过宽，包括退出应用 | 只授予用户在目标选择时确认的动作类别 |
| N5 | 未暴露最小化恢复、窗口移动/缩放、启动/激活应用、range、长按、后台点击；滚动量上限 ±1,000,000 与 Runtime 100,000 不一致 | `buildAction`、registry schema | 遇到最小化窗口无法恢复；超限请求直接报错 | 暴露 Runtime 已支持动作，参数上限从 Runtime capabilities 生成 |
| N6 | 分支落后 Nexus `main` 106 个提交；SDK 依赖为伪版本 | `go.mod` | 合入冲突与不可追溯依赖 | 变基到最新 `main`；SDK 固定到本仓精确 tag/commit |
| N7 | sidecar 打包位置、签名与系统权限归属未在真实机器验证；Windows 前台激活许可未实现 | `scripts/desktop/*`、权限设置页 | 权限可能记在 sidecar 而非 Nexus 名下；Windows 前台动作全部失败 | sidecar 置于 `Nexus.app` 内同 Team ID 签名；干净机器验证权限归属与撤销；Windows 主进程授予前台许可 |

### 23.5 性能问题（P 类，R1 修复）

- **P1 物理分辨率截图全流程**：RGBA PNG 全尺寸编码、写盘 `fsync`、宿主再读回并
  base64（[artifact_store.rs](../crates/runtime/src/artifact_store.rs) `write_image`；
  Nexus `readArtifact`）。历史 4K 诊断窗口捕获 p95 约 166 ms，超 100 ms 预算；
  模型 API 本来就会缩图，原图既耗时又浪费 token。
  **修复**：捕获配置直接输出模型预算尺寸；去掉 alpha；临时 artifact 不 `fsync`；
  宿主优先使用 checked take 取字节，避免二次路径读取。
- **P2 动作前整幅重截图复核**：指纹只用 32×32 亮度网格，却整幅捕获
  （[macos/mod.rs](../crates/platform/src/macos/mod.rs) `observation_is_current`）。
  历史 4K 诊断前台动作 p95 约 128 ms，超 60 ms 预算。
  **修复**：复核使用极小尺寸捕获或 warm stream 最新帧。
- **P3 捕获前内容枚举**：目标变化或缓存失效时每次查询 shareable content。
  **修复**：由 §2.6 的 warm stream 承担，枚举只在拓扑变化时发生。
- **P4 每步两次往返**：见 D5，由 `observe_after` 解决。
- **验收**：§16.1 新增预算在 4K 与 Retina 两种 runner 上通过，并绑定 source revision
  与 runtime digest。

### 23.6 Windows 测试重点（W 类）

除 B2、B3、B4、B6 外，Windows 实测优先覆盖：

- **W1 前台锁**：后台 sidecar 调用 `SetForegroundWindow` 常被系统拒绝
  （Windows input `activate`），表现为所有前台动作 `ForegroundRequired`；验证宿主授予
  前台许可后的成功率，不使用模拟按键等绕过手段作为静默回退。
- **W2 UIPI**：目标为管理员权限窗口时错误清晰、无部分派发。
- **W3 每显示器 DPI**：混合缩放、负坐标、跨屏窗口的坐标映射。
- **W4 WGC**：系统边框行为、Win10/Win11 差异；场景遮盖对 cloaked 窗口、
  其他虚拟桌面窗口和菜单类窗口的处理。
- **W5 CI**：`e5944fc` 暂停了 Windows native CI；R0 修复后恢复 Windows source gate，
  维护硬件 native 证据按修订合同采集。

### 23.7 范围收敛与原工作包去向

原 2026-10-26 的 10 个剩余工作包按以下方式处理：

| 原工作包 | 去向 |
| --- | --- |
| 1 CLI exit/error 合同 | 已实现，持续回归 |
| 2 可信宿主自动启动与撤销编排 | 并入 R2（Nexus 宿主生命周期）与 D4 紧急停止 |
| 3 运行中 adapter 撤销闭环 | 并入 D4 紧急停止合同，R1 |
| 4 registered HTTP integration | 冻结 |
| 5 CLI/MCP shared-operation conformance | R1 后按新合同执行 |
| 6 Codex acceptance 3/3 | R1 后执行，固定流程增加菜单与多显示器场景 |
| 7 native matrix | 按 §20.2 新场景重做 |
| 8 性能与 soak | 按 §16 新预算重做（P1–P4） |
| 9 签名候选流水线 | R2 内部试用通过后推进 |
| 10 渠道 clean-host | 冻结（Nexus 内置 sidecar 的 clean-host 由 N7 覆盖） |

trycua 对照（§8.10）结论不变：基础观察/动作范围已大量重叠，差距在后台输入广度、
Linux、安装接入与维护验证；Browser DOM/CDP 与模型 loop 是永久边界。

### 23.8 既有证据与效力

- `target/native-evidence/macos-apple-silicon/validation.json` 的 15 个场景组通过，
  覆盖混合 DPI、多显示器、负坐标、遮挡/最小化、前台/语义动作、同请求对账、
  会话过期、陈旧观察和 artifact 清理。它们验证的是旧的单窗口观察设计，修订后的
  场景观察、hit-test、presence 需要新证据。
- `restart.json`、`fault-preflight.json`、`fault-dispatch.json` 的重启与故障场景
  仍有效，但不替代权限拒绝/撤销及 background/restoration 验收。
- `benchmark.json` 未绑定 source revision/runtime SHA-256；
  `benchmark-4k-sck-diagnostic.json` 的截图与前台动作预算未通过（根因见 P1、P2）；
  `soak-4k-sck-5m.json` 有内存单调增长，其他诊断运行无增长，引入 warm stream 后
  必须重新做 soak 归因。
- [平台验证记录](platform-validation.md) 中真实 Codex 经 MCP 的开发验证仍有参考价值，
  不等于签名候选 3/3 验收。
- 本地 `target/` 报告不在版本控制中，`passed` 或 `accepted_release_evidence` 字段
  不能直接认定当前提交已通过。

### 23.9 本次修订的质量门禁

本次只修改本 PRD，不修改源码与其他文档；技术文档在 R1 实现时同步更新。
提交前运行 `make check`，Markdown 链接检查须通过。

### 23.10 第二轮审查：`47800d9` 与 Nexus `64cf8e9e8` 的修复要求

审查对象：nexus-cua 提交 `47800d9`（R0 大部分与 R1 会话热流）和 Nexus `cua` 分支
提交 `64cf8e9e8`。`make check` 通过；B1–B6、B8 的源码修复方向正确，协议级
`NormalizedPoint`、滚动方向、`mod`、布局解析、Windows 扩展键与滚轮换算、输入
节流、会话捕获所有权撤销均保留。以下 C1–C12 是必须修复的问题，编号供提交信息
和本节状态更新引用。

**执行规则**

- 每个 C 项单独提交，提交信息引用编号；源码、协议 schema、三套 SDK、Skill、
  [Protocol](specs/protocol-v1.md)、[Runtime Contract](specs/runtime-contract-v1.md)
  和 `CHANGELOG.md` 在同一提交内同步。
- 每项必须新增确定性测试；需要真实桌面的验收写入
  [平台验证记录](platform-validation.md)，没有原生证据时只能标"源码已修复"。
- 不得新增空闲轮询、无界队列、无界重试或跨 actor 移动原生对象；
  `make check PYTHON=<python3.10+>` 通过后才能提交。
- 推送远端、改动 Nexus 以外的仓库或修改 CI secret 前先征得用户同意。

**建议顺序**：C5、C10 → C1、C2 → C3 → C6、C7、C8 → C4 → C9 → C11 → C12。

#### C1 窗口列表与观察不得依赖辅助功能（高）

实施状态：源码已修复；增加无权限、80 窗口、身份失败和超时的确定性覆盖。原生权限撤销与不同 AX 标题验收待执行。

- **问题**：`list_windows` 对每个窗口调用 AX 查询最小化状态；无辅助功能权限时整个
  调用失败；AX 查找失败（含系统窗口标题与 AX 标题不一致、身份歧义）时窗口被
  `continue` 静默丢弃；单应用超过 64 个窗口直接 `Busy`。带截图的 `observe` 即使
  `accessibility = disabled` 也强制 AX 查询。位置：
  [macos/mod.rs](../crates/platform/src/macos/mod.rs) `list_windows`、
  `observe_window_owned`；[semantic.rs](../crates/platform/src/macos/semantic.rs)
  `window_minimized`、`find_window`。这会让"看不到 app"更严重。
- **修复**：
  1. 窗口集合只由系统窗口列表决定，AX 只做状态增强，任何 AX 失败都不得删除窗口
     或让列表失败；恢复原有的 `visible || !title.is_empty()` 噪声过滤。
  2. 协议把 `WindowSummary.minimized: bool` 替换为封闭枚举 `state`，本项先提供
     `normal | minimized | offscreen | unknown`（D2 再追加 `app_hidden`、
     `other_space`、`fullscreen_space`）。AX 不可用、超时、歧义或标题不一致时为
     `unknown`，不可见时至少为 `offscreen`。
  3. 未授予辅助功能权限时完全不调用 AX。
  4. AX 增强按 z-order 只处理前 16 个窗口，共享 750 ms 预算；超出预算或数量的
     窗口为 `unknown`，不报错。
  5. `observe` 的最小化前置检查：有 AX 权限时读 `AXMinimized`；无权限时以
     系统窗口列表的可见性判断，不可见返回 `TargetUnavailable` 并建议
     `restore_window`/`activate_application`。
- **验收**：撤销辅助功能权限后 `list_windows` 与仅截图 `observe` 成功；fixture
  增加一个 `accessibilityTitle` 与窗口标题不同的窗口，它仍出现在列表中且状态为
  `unknown` 或正确值；单应用 80 个窗口时列表成功返回。

#### C2 热流在静态画面下必须可用（高）

实施状态：源码已修复；增加静态完整帧复用、停止后拒绝缓存、首帧超时回退的确定性测试。刷新等待上限 100ms；原生静止 5 秒、内容更新与 p95 验收待执行。

- **问题**：`CaptureStream::snapshot` 要求请求之后收到新的帧回调，否则 1 秒后
  报错并拆除流（[stream.rs](../crates/platform/src/macos/stream.rs)、
  [capture.rs](../crates/platform/src/macos/capture.rs) `stream_capture`）。
  ScreenCaptureKit 在内容不变时可能不再回调，动作前视觉复核也走这条路径，
  静止窗口上的观察和动作都可能失败。
- **修复**：
  1. 帧槽记录"最近完整帧"和"流状态"（运行/停止/目标不可用）。流运行中且已有
     完整帧时，等待不超过两个帧间隔（200 ms）仍无回调，即视为内容未变并返回
     最近帧。
  2. 为 `SCStream` 设置 delegate，收到停止或错误时标记失效；下一次请求重建，
     不在请求路径上等满 1 秒。
  3. 流启动后 1 秒内没有首帧时，本次请求回退到一次性截图，流保留，不把错误
     暴露给调用方。
  4. 动作前复核直接使用最近帧，不等待新回调。
- **验收**：静止窗口观察后等 5 秒再观察并执行动作，二者都成功，观察 p95 ≤ 150 ms；
  内容变化后 200 ms 内的观察反映新内容；目标窗口关闭时返回明确的
  `TargetUnavailable`，并释放流。

#### C3 Nexus 分支必须在不依赖本机 `go.work` 时可构建（高）

实施状态：已完成。经用户批准推送 Runtime 236fb58；Nexus d2d1f504d 固定精确 SDK 伪版本与校验和，新增独立构建目标并在后端 CI 禁用 workspace。干净 clone 的 GOWORK=off 全量 Go 构建和两个 CUA 接入包测试通过；Nexus 分支尚未推送。

- **问题**：Nexus `go.mod` 仍锁定 2026-10-20 的旧 SDK 伪版本，能编译只是因为有
  被 gitignore 的本地 `go.work` 指向本机 nexus-cua；`GOWORK=off go build` 报
  `undefined: nexuscua.NormalizedPoint`。
- **修复**：征得用户同意后推送 nexus-cua；Nexus `go.mod` 固定到该提交的精确版本
  并更新 `go.sum`；Nexus 增加 `GOWORK=off` 的构建/测试检查（Makefile 目标或 CI 步骤）；
  永不提交 `go.work`。
- **验收**：在干净 clone 中 `GOWORK=off go build ./... && GOWORK=off go test ./internal/service/computeruse/... ./internal/mcp/command/computer/...` 通过。

#### C4 热流按需转换并在捕获阶段缩放（中，含 P1）

实施状态：源码已修复；macOS 原生缓冲留在输出队列，按请求转换；流和一次性捕获按默认 1568 长边配置。RGB PNG 与缩放映射有确定性测试。Windows WGC 保持原生采集尺寸，在编码阶段有界缩放。原生视频 60 秒 CPU、4K PNG 大小与点击验收仍待执行。

- **问题**：每个完整帧都在回调队列上做全分辨率 BGRA→RGBA 转换与反预乘，
  10 fps 持续进行，与是否有人观察无关；取帧时再完整复制一份；流尺寸等于物理
  分辨率。窗口内播放视频时 CPU 持续升高，内存翻倍。
- **修复**：
  1. 帧槽只保留最新一帧的原生像素缓冲引用（替换时释放旧的），转换只在
     `snapshot` 时由捕获 actor 执行，且只转换一次；遵守"原生对象留在所属
     actor"规则，像素缓冲不离开捕获侧。
  2. `RuntimeConfig` 增加模型图像长边上限（默认 1568），`serve` 与 MCP 提供同名
     参数；流配置直接输出该尺寸，`ScreenshotMapping` 反映实际像素尺寸。
  3. 产物 PNG 去掉 alpha 通道（RGB）。
  4. 一次性截图路径同样按该上限配置输出尺寸。
- **验收**：session 打开但无观察请求时，目标窗口播放视频 60 秒，sidecar 平均 CPU
  < 2%；4K 窗口观察输出长边 ≤ 1568，编码后 ≤ 1 MiB；归一化点击在缩放后仍命中
  fixture 目标。

#### C5 捕获 actor 禁止空闲轮询（中）

实施状态：已去除周期轮询，改为命令/撤销通知/最近流到期唤醒；新增队列满时撤销不丢失等确定性测试。源码检查通过；当前优化构建的 5 分钟无 session 诊断测得 CPU 0.0%、文件描述符 22→22、无持续内存增长。原生 100ms 释放及正式 runner 验收仍待采集。

- **问题**：`actor_loop` 用 `recv_timeout(100ms)` 永久轮询，即使没有任何 session
  （[capture.rs](../crates/platform/src/macos/capture.rs)），违反"无请求时禁止
  active polling"。
- **修复**：没有热流时阻塞 `recv`；有热流时超时设为最早一个流的空闲到期时间。
  `session_closed` 通过 `try_send` 向 actor 发送 `Prune` 命令，使撤销立即生效且
  不依赖轮询；通道满时由下一次命令或到期唤醒处理，不得阻塞调用方。
- **验收**：无 session 空闲 5 分钟，捕获线程无周期唤醒（`sample` 或
  native-harness idle 证据）；session 关闭后 100 ms 内释放流（保留现有所有权测试）。

#### C6 指针命中检查改为窗口层级判断，并覆盖全部像素动作（中）

实施状态：源码已修复；检查覆盖两平台全部指定像素动作并移入输入 actor，实机发现 Dock/Stage Manager 的全屏穿透层会被矩形排序误判，改用主线程 AppKit 真实 mouse-down 命中，原生引用不离开所属线程。删除仅复述动作枚举/矩形排序的测试。当前 Mac 原生核心动作、窗口替换和会话过期路径通过；完整回归仍阻塞于 fixture 最小化，同进程菜单及自绘滚动尚待专门验收。

- **问题**：macOS 滚动前不移动指针，只依赖事件位置字段；命中检查使用 AX
  `AXUIElementCopyElementAtPosition`，在 AX 支持差的应用（Java/Qt/游戏/画布）中
  失败，同一应用的弹出菜单也会因"窗口身份不符"被拒；Windows 要求根窗口等于
  目标窗口，同样拒绝同进程弹窗；点击、长按、拖拽没有命中检查。
- **修复**：
  1. macOS：通过有界主线程队列调用 AppKit 实际 mouse-down 窗口命中，输入 actor
     用该窗口的 CG 元数据核验授权进程（跳过 runtime 自身窗口）。原先矩形排序
     方案会把 Dock/Stage Manager 的全屏穿透层误当遮挡，已由真实命中替代。
     不依赖 AX，不按应用名或窗口层级建立绕过白名单。
  2. Windows：`WindowFromPoint` 后取所属进程，要求等于授权进程，不再要求根窗口
     相等。
  3. 命中检查放在输入 actor 上、紧挨派发之前执行。适用于前台 `click_point`、
     `press_point`、`scroll`，以及 `drag` 的起点和终点；`move_pointer` 与后台点击
     不检查。失败返回 `not_dispatched`，建议 `observe_window`。
  4. macOS 滚动前先发送一次指针移动到目标点，与点击、Windows 一致。
- **验收**：另一应用窗口覆盖目标点时，点击/滚动/拖拽被拒且无副作用；同一应用
  弹出菜单上的点击成功；fixture 中无 AX 信息的自绘视图可滚动。

#### C7 删除被忽略的 `Scroll.element_ref`（中）

实施状态：源码已修复；字段已删除，兼容性 invalid fixture 验证即使传 null 也拒绝。

- **问题**：协议仍接受并解析 `Scroll.element_ref`，但两个平台驱动都直接丢弃，
  文档未说明。
- **修复**：本轮协议允许重置，直接从 protocol、schema、三套 SDK、MCP、CLI 和
  Skill 中删除该字段；语义滚动将来作为独立动作设计。
- **验收**：schema 不再包含该字段；带该字段的请求因未知字段被拒（兼容性 fixture
  增加一条 invalid 用例）。

#### C8 归一化坐标支持亚单位精度（中）

实施状态：源码已修复；4K/1366×768 每个轴像素往返测试及非有限值拒绝测试已加入。原生 4× zoom 小目标命中仍待验收。

- **问题**：`click_view_normalized` 把裁剪视图中的点换算回整图 `0..1000` 整数，
  4K 宽窗口误差约 ±2 px，削弱了 zoom 的意义。
- **修复**：`NormalizedPoint` 的 `x/y` 改为有限 `f64`，闭区间 `[0, 1000]`，由
  Runtime 统一舍入到像素；面向模型的工具仍使用整数输入；zoom 路径传入小数。
  三套 SDK 与 schema 同步；拒绝 NaN/Infinity。
- **验收**：对 3840×2160 和 1366×768 图像中的每个采样像素，`from_pixel` 后再
  `to_pixel` 得到同一像素（属性测试）；zoom 4× 后点击 4×4 px 目标命中。

#### C9 Nexus 授权与动作暴露（中，Nexus 仓）

实施状态：Runtime 已增加与校验共享的 action_limits，三 SDK/schema 同步；Nexus 目标授权、回合时限、敏感动作审批、恢复/窗口/数值/长按/启动入口已实现；Runtime 与 Nexus 全量 make check 均已通过；Nexus C9 为 4ebe4ec12。真实 Nexus 权限 UI 和恢复窗口验收仍待执行；固定 SDK 提交属于 C3。

- **问题**：TTL 改为固定 30 分钟但仍不续期、不与回合绑定；前台输入仍无条件授予；
  逐动作审批仍位于观察之后（N3）；恢复最小化等动作仍未暴露（N5）。
- **修复**：
  1. `select_target` 通过现有权限通道发起一次授权，内容为应用身份、动作类别、
     是否允许前台输入、时长；session 只授予确认的类别和前台开关。
  2. 常规动作不再逐次审批；退出应用、剪贴板写入及宿主策略标记的动作逐次审批，
     审批通过后若观察已失效，直接返回 `stale_observation` 并提示重新观察，
     不自动重放。
  3. session TTL 取回合剩余上限与 Runtime 上限的较小值，回合结束立即关闭；
     回合中过期时返回明确错误并要求重新 `select_target`，不静默重开。
  4. 暴露 `restore_window`（`set_window_minimized=false`）、`set_window_bounds`、
     `set_range_value`、`press_point`、可启动应用列表与启动；参数上限从 Runtime
     capabilities 生成。
- **验收**：常规点击/输入不弹审批；未授权类别被 Runtime 拒绝；最小化窗口可经
  `restore_window` 恢复后观察；回合结束后旧 session 不可用。

#### C10 删除无效 CLI 参数（低）

实施状态：源码已修复；两个子命令的解析回归测试拒绝此参数，serve/MCP 保留配置。

- **问题**：`nexuscua request` 与 `mutation reconcile` 新增的
  `--max-observation-age-seconds` 没有任何效果
  （[commands.rs](../crates/cli/src/commands.rs) `RequestArgs`、`ReconcileArgs`）。
- **修复**：删除这两个参数，仅保留 `serve` 与 MCP 上的参数。
- **验收**：CLI 解析测试断言这两个子命令拒绝该参数。

#### C11 窗口枚举只做一次（低）

实施状态：源码已修复；多应用 session 的驱动调用计数和授权集合测试已加入。

- **问题**：Runtime `list_windows` 对每个授权应用调用一次驱动，每次都完整枚举
  系统窗口。
- **修复**：驱动接口改为接受授权应用 key 集合，一次枚举后过滤；AX 增强（C1）
  只作用于授权应用的窗口。
- **验收**：多应用 session 的 `list_windows` 只触发一次系统枚举（驱动计数测试）。

#### C12 Windows 显示器捕获 `0x80070057`（Windows）

2026-11-03 的源码与测试机复核见 [Windows readiness review](platform-validation.md#windows-readiness-review-2026-11-03)。远端现有测试二进制仍为 0.1.0；以下原生结果是历史诊断，不能当作 rc.4 验收。当前优先补齐原生接管/撤销检查，再验证后台点击效果与 Nexus 前台许可，随后补动作后观察及固定版本打包验收。

实施状态：已补当前拓扑解析、PMv2 验证、WGC 支持检查和无内容最小诊断程序。Windows 11 build 26300 上最小程序的窗口/显示器创建均成功；完整 runtime 已通过显示器观察；进一步补齐 UIA pattern 缓存属性，并修复 WGC 在无画面变化的动作后等待不到新帧。当前原生语义动作、鼠标移动和前台点击通过，后台点击未改变 fixture 计数，整体原生验收未通过。旧 `0x80070057` 根因尚未被单变量复现确认。

- **问题**：活动控制台 session 中 `observe_display` 的 WGC `CreateForMonitor`
  返回 `E_INVALIDARG`，原生回归无法进入窗口动作阶段。
- **修复步骤**：
  1. 在调用点重新解析 `HMONITOR`（`EnumDisplayMonitors`/`MonitorFromPoint`），
     不复用跨显示拓扑变化的缓存值，并记录（不含内容的）显示器设备名与句柄有效性；
  2. 确认调用线程已初始化 WinRT/COM 且进程为 Per-Monitor-V2 DPI 感知；
  3. 调用前检查 `GraphicsCaptureSession::IsSupported()`，不支持时返回 `Unsupported`；
  4. 在同一机器上用最小复现程序对比窗口捕获与显示器捕获，定位后修复，
     不得静默回退到旧捕获 API。
- **验收**：Windows 原生回归的显示器观察、窗口观察和动作场景通过，证据写入
  [平台验证记录](platform-validation.md)。

#### 本轮仍未开始、不在 C 项内的工作

应用场景观察（D1）、窗口状态完整枚举与激活应用（D2）、显示器身份（D3）、
presence/紧急停止/用户接管与系统指示验收（D4）、`observe_after`（D5）、
低分辨率复核（P2）、输入法组字检测（B7 剩余部分），仍按 §21 R1 执行。

### 23.11 2026-11-03 macOS 内部试用收口（进行中）

本轮按用户确认先收口 macOS，保留 §21 对 remote/渠道扩展的冻结。
当前改动尚未提交或推送，不能将本节视为整体验收完成。

- 应用发现改用系统运行应用列表，原生对象保留在所属线程；主线程按请求有界处理
  workspace 通知，无空闲轮询。实机验证了服务已发现一次后新启动无窗口 fixture
  的发现、会话准入、空窗口列表和退出后消失。
- 新增 `application_scene` capture capability 与 `observe_application_scene`，
  CLI/MCP 和三 SDK 同步。截图仅包含锚点所在显示器上的授权应用及同进程菜单，
  AX 仍描述锚点窗口；跨进程辅助面板和多显示器联合场景尚未覆盖。
  本机已观察到原生 popup，并用场景坐标点击 Menu Beta 使计数 0→1。
- 台前调度已确认是必须处理的真实环境：用户看到窗口进入左侧栏，原生读取为
  `AXMinimized=false`、AX 960×748、CG/SC 缩略图 104×134。不能把此状态当作标准
  minimized，也不能仅因 AXMinimized=false 就报告 normal。
- 显式 `focus_window` 先核验进程代际再激活，等待几何恢复后仅派发一次 raise；
  读取绝不自动激活。本机恢复与恢复后的捕获连续 3/3 通过；标准最小化验收仍独立保留。
- 会话关闭/到期保护传入原生执行线程，在派发前重新核验；长按和拖动中断后释放鼠标。
  确定性测试覆盖预检期间关闭后不派发；实机三秒长按在确认按下后关闭会话，
  23.5ms 后检测到释放，返回 `session_unavailable` / `indeterminate`。
  此单次诊断不是停止延迟保证，也不代表宿主紧急停止或用户接管已完成。
- presence/用户接管/紧急停止、完整窗口与显示器身份、动作后观察和 Nexus 三类真实
  任务仍未完成；不得将本轮菜单诊断替代 Nexus 或 release 验收。

#### 用户接管后续实现（2026-11-03）

- macOS 前台授权会话在准入时安装只读事件监听，判断事件类型、来源 PID 和
  自有合成标记。rc.3 仅额外保留有界的当前键/按钮按下状态、修饰键标志和最近输入时间，
  不读取或保留按键文字、位置或事件历史。两次动作间的输入等待空闲并要求新观察，
  不撤销原授权；原生动作执行中的输入锁定为 `user_active`，需用户明确继续后重新授权。
- 原生线程在派发前及长动作中检查接管标志；停止后只释放已按下的合成输入。
  事件源显式允许物理输入通过，关闭合成事件抑制。监听权限不足或失效则拒绝继续。
- 本机实测外部进程鼠标事件中断通过；用户配合的真实鼠标长按中断也通过，并验证
  同会话后续动作被拒绝。语音提示与人工反应时间计入脚本耗时，不能作为停止延迟。
- 键盘/安全输入、多设备、宿主 UI 和完整平台验收仍需区分；本次不包含停止入口、
  presence 或 Nexus 固定 SDK 版本接入。

#### 性能与测试预发布（2026-11-03）

- 热捕获流的动作前复核在原生串行队列上仅复制 32×32 个采样像素，保持原有
  视觉指纹算法；完整观察仍保留正常截图。冷启动/一次性截图路径不在此次优化内。
- `observe_after` 已进入 capability、Runtime、通用 MCP 动作和三 SDK；普通动作
  不传该选项仍保持原行为。后续观察检查同一读权限，最多三次视觉采样后走正常
  观察路径；观察失败单独返回 `observation_error`，不能据此重做已执行动作。
- 新观察可作为下一次动作的 guard，旧观察始终作废；相同请求 ID 对账保留原结果。
  性能脚本只报告有界 fixture 诊断，不替代长时间负载、用户接管或业务完成验收。
- `v0.2.0-rc.1` 已作为 macOS Apple Silicon 测试预发布，源码固定于
  `7af97507c55153d262c4dbfd14c7bfa5bdd52ce9`，附校验和、SBOM 和第三方声明；
  不宣称正式签名/公证、干净机器、双平台或完整 release 验收。
- Nexus `1cca54d53d13ead7fef36535202fb2d3dc9dcd05` 已固定同版本 Go SDK，
  本地预览包嵌入发布的原始 runtime，使用独立 bundle ID 和数据目录。打包后真实
  service→SDK→runtime 测试通过授权、截图交付、连续两步动作、重复请求对账和关闭；
  本机设置页确认录屏、辅助功能和输入权限可用；独立预览包的启动、窗口路由与退出
  smoke 通过。生产安装器的正式版本校验保持不变。
- GLM-4.5 经实际 Nexus 聊天完成 fixture 两次语义按钮动作，动作后新观察与独立窗口
  读取均确认每步计数加一，最后关闭会话。联调发现 Nexus 将截图 Base64 放入文本结果，
  已改用 MCP image block，并清理持久化消息中的动作后截图和 AX 树。相同提示词的
  两轮受控诊断由 30 次工具调用/222.9 秒变为 15 次/90.2 秒，后一次无工具错误；
  耗时包含模型响应与人工授权等待，不是通用加速承诺，也不替代真实业务或完整平台验收。
- 解锁桌面后，在同一本机、同一 fixture 上顺序执行三个各 12 次的诊断：基线
  `7e5e261` 分开执行动作与观察的中位耗时为 254.36 ms；发布版相同路径为
  209.22 ms；发布版合并接口包含有界稳定等待，为 303.55 ms。三个批次均核对
  每步只增加一次计数，合并批次另核对相同请求 ID 不重复执行。样本不足以证明通用
  加速比例，合并接口也不保证低于分开调用的总延迟。

## 24. 保留的技术文档

本 PRD 不重复展开以下仍有执行价值的技术资料：

- [Architecture](architecture.md)：依赖方向、actor 和数据生命周期；
- [Protocol v1](specs/protocol-v1.md)：wire schema 语义；
- [Runtime Contract v1](specs/runtime-contract-v1.md)：运行时、失败和性能合同；
- [Execution Environments](specs/execution-environments.md)：桌面 worker 与 VM 边界；
- [Compatibility](compatibility.md)：版本兼容政策；
- [Runtime Policy](runtime-policy.md)：managed/user policy 组合；
- [Sidecar Supervision](sidecar-supervision.md)：宿主监督和重启；
- [Platform Validation](platform-validation.md)：真实平台证据；
- [Codex MCP dogfood](codex-mcp.md)：当前 source-build runbook；
- [Python Quickstart](quickstart-python.md)：SDK 使用示例；
- [Dependency Policy](dependency-policy.md)：依赖治理。

这些文档只负责各自技术细节，不能再引入另一套产品命名、安装 profile、Skill
策略、路线图或发布逻辑。

#### 台前调度恢复补充（rc.4）

AX 窗口匹配在 restore/resize/raise 的首个原生写入之前失败时，必须返回
`not_dispatched`；不能让宿主永久等待一个未执行动作的对账。匹配不到当前窗口时
建议对同一窗口重新做纯元数据观察并明确 `focus_window`。激活应用之后再失败仍是
`indeterminate`，不能降级为未派发。`validate_macos_stage_manager.py` 覆盖缩略状态
下 restore 被拒后，沿用相同授权聚焦并重新截图；此诊断不替代完整发布验收。
