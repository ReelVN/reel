# Reel 引擎架构设计
设计版本 1.1 · 开发基线，接口实现状态见正文。

[文档目录](README.md) · [作者指南 中文](guide/zh-CN.md) · [Creator guide English](guide/en.md)

## 目标与文档边界
Reel 面向视觉小说，以统一舞台组合剧情画面、UI、文字和效果，同一组节点可组成对话框、图片按钮、菜单和演出，再封装为作者组件。引擎复用 Masonry 的控件能力和 Kira 的音频能力，配套语言 Aside 以 Quirrel 为参照，用 Rust 实现编译器和 VM。
本文规定运行时所有权、输入与渲染、媒体、持久化和开发顺序。《Aside VM 与完整语言规范》负责语法、值模型、执行帧、回收和语言兼容；《Reel 游戏开发指南》负责作者接口、组件组合与示例。跨文档的等待、绑定和恢复语义必须一致。三份文档交叉审查后成为 reel 仓库 docs/ 下的设计基线。预算与性能数字均为验收目标，尚未形成实测结论。

## 一 产品范围与实现原则
剧情按阅读顺序运行，在对话和选择处等待；角色身份、历史记录与显示组件分开。画面和 UI 共用节点、资源、布局、变换、裁剪和效果组。长期存在的节点通过属性更新，避免每次输入重建整个界面。参考 Ren’Py 的交互与屏幕复用经验。[1][2][3]
核心提供存档、设置、历史、对话与选择的数据模型及操作接口。标题页、设置页和存档页由作者组合；项目脚手架只生成配置和入口，不附带一套完整内置 UI。用针对性契约用例验证这些能力，作者文档中的片段用于说明组合方式。
初期使用一个工作区，先落地默认 winit 窗口适配和 wgpu 渲染器，窗口管理与渲染后端可以独立选择。完整目标包含视频、可替换的软件/硬件后端、文字与本地化、可访问性、二维/透视演出及扩展服务；阶段安排按依赖推进。静态子树缓存和增量快照是保持同一行为的优化。现有 ons-rs、krkr-rs 用作结构参考。[4]

## 二 模块职责与依赖
工作区按职责划分，语言包可独立发布。
- reel-core：平台无关的资源/请求身份、预算/结果类型、AssetIo、Clock 与 WorkExecutor 契约；只含共享数据和基础服务接口。
- aside-vm：语言值、对象、调用帧、模块与执行接口。
- aside-compiler：解析、分析、字节码及源码定位，依赖 VM 公共格式。
- reel-ui：控件、布局、属性、交互与舞台桥接。core/app/layout/properties/widgets/theme 管理 UI 基础，bridge/frame/resources 管理呈现接受、可移植帧与资源租约；passes/tree_arena 为私有实现。
- reel-render：后端接口、资源上传、缓存与分组合成，读取 stage 提交结果。
- reel-media：Kira 适配和播放控制，视频作为可选模块。
- reel-engine：会话、剧情任务、宿主绑定、资源与持久化协调。
- reel-player：平台适配、窗口与事件循环，组装引擎、渲染和媒体输出。
- reel-cli：检查、开发运行和构建，调用编译器与播放器库入口。
依赖箭头固定为 stage→core，render→stage/core，media→core，engine→aside-vm/stage/media/core，player→engine/render/core，cli→compiler/player，compiler→aside-vm。Aside 保持独立于所有 reel 包。语言包不依赖窗口、舞台或媒体。业务数据类型只在所属模块定义一次，跨线程传递拥有自身数据的消息或线程安全只读缓冲，不传 VM 堆引用。渲染器通过资源 ID 与版本取得内容。
通用依赖直接使用锁定版本，不复制进仓库。Parley、winit、wgpu、image、serde、atomic-write-file 与 codespan-reporting 按普通依赖管理。需修改的代码从明确来源借用后在仓库内维护；其余依赖继续锁定引用。文本与集合优先复用 unicode-segmentation、unicode-normalization、indexmap；数学使用 glam；VM 对象槽存储复用 slotmap 1.1.1，增量追踪契约见语言文档；语言工具采用 Logos、rowan、单一递归下降/Pratt parser、codespan-reporting 和 lsp-server。视频默认 ffmpeg-next/ffmpeg-sys-next 与锁定的 FFmpeg；资源使用独立 zstd 块、Stored 媒体与 BLAKE3；C ABI 声明用 cbindgen，确需无锁 PCM 桥时使用 rtrb。GStreamer、另一套 parser、libloading 和替代压缩算法只有明确替换或平台理由才进入配置。借用的 Masonry 控件、布局与呈现几何代码整合到 reel-ui 后由 Reel 自主修改和维护，不以维持上游包图、同步或贡献为目标。Kira 0.12.5 的本地修改集中在退休 Sound 的有界纯移动接口和退休队列容量检查，支持后台 PCM 释放；其内部 atomic-arena 继续使用普通依赖。
third_party/quirrel 保存 Quirrel 4.41 的固定源码与对应测试快照，基线为 19f17df9b6ee43c91ef5c635fd45970ec76c5dbe，用于规范核对与差分测试。它属于开发工具，不链接进发行播放器；Aside 编译器与 VM 是工作区自己的 Rust 实现。[6]
UI 代码最初借用自 Masonry 基线 271a27a6d4a930f7878d404f9014e3c50a3a9b88。[5] 当前仅维护 crates/reel-ui 一个 UI crate，版本跟随 Reel workspace；原 masonry、masonry_core、tree_arena、reel-stage 的实现按职责合为内部模块，不保留兼容转发包。安全树为私有模块，文档截图直接引用，不再保留 include_doc_path 宏包。许可、版权及必要来源说明保留；不要求与上游目录、版本或源码哈希持续一致。reel-render/reel-player 分别承担后端与平台组装，UI 不反向依赖它们。
仓库内第三方源码采用固定快照，不递归保留嵌套 .git。每份快照记录上游地址、提交、版本、导入日期与本地修改，保留并核对许可证及版权声明；更新时可追溯差异。Quirrel 行为对照和 Kira 0.12.5 仍保留各自适用的来源记录。reel-ui 已是 Reel 自维护的借用代码，历史来源不再约束当前模块组织。Kira 当前保留原始许可证、导入版本的格式配置与已批准的修改记录；这些来源记录不构成向上游同步或贡献的承诺；音频清理线程和预算策略留在 reel-media 适配层。
播放器的 source 能力选择性引入编译器；精简版本只加载字节码。hot-reload 只开启模块替换协议，可接收外部编译产物。编译器、监视器和对照运行时留在开发侧，精简构建检查实际依赖树。[7]

## 三 平台接口与移植边界
窗口和渲染分别选择，默认组合为 WinitHost 与 WgpuBackend。reel-player/platform 定义 PlatformHost，reel-render 定义 RenderBackend，reel-media 定义 AudioOutput；AssetIo、Clock、WorkExecutor 在 reel-core 中定义。player 组装适配器并转换输入，engine 与 reel-ui 不反向依赖 player。UI 控件与布局同窗口外壳和 GPU 后端分开，以依赖检查和限定构建验证隔离。
原生窗口或固定显示目标的创建、表面来源协商与销毁留在具体 player 组装代码，不进入公共 trait。可借用或包装原始句柄，但有效期必须覆盖表面使用期；先停止提交并释放表面，再销毁显示目标。winit 事件、wgpu 设备与纹理、Vita 原生句柄都不得进入会话、RenderFrame 或 DSL。每个后端负责上传、格式转换、离屏目标、设备重建和完成后回收，不能仅替换最终绘制函数。
Cargo features 决定编入哪些适配器，运行参数只在已编入且能力匹配的组合中选择。游戏包声明必需能力；缺失时报错，有声明的低质量后备才可降级。reel-ui 与默认外壳、Kira 与默认输出的目标依赖都要检查，关闭桌面 feature 后不能仍被传递依赖强制引入。不建设动态插件系统或通用平台框架。
以下 Rust 签名规定模块边界，自定义类型是引擎自有数据；不统一要求 Send + Sync，只约束实际跨线程的数据。下文给出类型的最低字段与结果语义，D01 完成对应定义与契约测试。
```rust
pub trait PlatformHost {
    fn capabilities(&self) -> PlatformCaps;
    fn request_redraw(&mut self);
    fn set_wake_deadline(&mut self, when: Option<MonoTime>);
    fn set_text_input(&mut self, state: TextInputState)
        -> Result<(), PlatformError>;
    fn clipboard(&mut self, request: ClipboardRequest)
        -> Result<ClipboardId, Rejected<ClipboardRequest>>;
    fn update_accessibility(&mut self, update: AccessibilityUpdate)
        -> Result<(), Rejected<AccessibilityUpdate>>;
    fn request_exit(&mut self);
}
pub trait Clock {
    fn monotonic_now(&self) -> MonoTime;
    fn wall_time(&self) -> Option<UnixMillis>;
}
pub trait RenderBackend {
    fn capabilities(&self) -> RenderCaps;
    fn configure(&mut self, info: SurfaceInfo) -> Result<(), RenderError>;
    fn upload(&mut self, batch: UploadBatch)
        -> Result<UploadId, Rejected<UploadBatch>>;
    fn render_present(&mut self, frame: &RenderFrame)
        -> Result<PresentOutcome, RenderError>;
    fn retire(&mut self, resources: &[ResourceVersion]);
    fn poll(&mut self, out: &mut Vec<RenderEvent>, limit: usize);
    fn suspend(&mut self);
}
pub type PcmFill = Box<dyn FnMut(&mut [f32]) + Send + 'static>;
pub trait AudioOutput {
    fn capabilities(&self) -> AudioCaps;
    fn configure(&mut self, request: AudioRequest)
        -> Result<AudioFormat, AudioError>;
    fn start(&mut self, fill: PcmFill) -> Result<(), Rejected<PcmFill>>;
    fn suspend(&mut self) -> Result<(), AudioError>;
    fn resume(&mut self) -> Result<(), AudioError>;
    fn status(&self) -> AudioStatus;
    fn clock_observation(&self) -> AudioClockObservation;
    fn stop(&mut self) -> Result<(), AudioError>;
}
pub trait AssetIo {
    fn capabilities(&self) -> IoCaps;
    fn describe(&mut self, request: DescribeRequest)
        -> Result<IoId, Rejected<DescribeRequest>>;
    fn read(&mut self, request: ReadRequest)
        -> Result<IoId, Rejected<ReadRequest>>;
    fn replace_save(&mut self, request: SaveWrite)
        -> Result<IoId, Rejected<SaveWrite>>;
    fn cancel(&mut self, id: IoId);
    fn poll(&mut self, out: &mut Vec<IoEvent>, limit: usize);
}
pub trait WorkExecutor {
    fn capabilities(&self) -> WorkCaps;
    fn submit(&mut self, work: WorkRequest)
        -> Result<WorkId, Rejected<WorkRequest>>;
    fn cancel(&mut self, id: WorkId);
    fn advance(&mut self, budget: WorkBudget) -> WorkProgress;
    fn drain(&mut self, out: &mut Vec<WorkEvent>, limit: usize);
}
```
基础类型与结果约定
以下字段是接口的最低语义要求，D01 将其落实为 Rust 定义与测试；字段名称可随编码规范调整，含义、单位、所有权与终态规则不得由各代理自行重定。
- 身份：各类请求 ID、FrameId、表面代数使用相互独立的 u64 包装类型。Scope 为 Session(session_id, epoch, owner_id) 或 Process；保存写盘使用 Process。MonoTime 是单调纳秒 u64，UnixMillis 是可缺失的 UTC 毫秒 i64，均不含本机指针。
- Rejected<T> 保存原始 request:T 与 reason。reason 至少区分 Busy、Unsupported、InvalidRequest、Unavailable、OutOfMemory 和服务故障，附拥有型诊断文字；只有 Busy/Unavailable 等明确可重试原因允许原请求重试。拒绝不产生已接受 ID 或业务副作用。已接受请求的终态为 Completed(payload)、Cancelled、Failed(error)，结果记录包含请求 ID 与 Scope。Cancelled 用于释放与记账，不重新唤醒已取消业务。
- QueueLimits 含 max_requests:u32、max_bytes:u64；BudgetTicket 含预算类别和 bytes:u64，随拥有的数据转移，最终释放一次。接受请求时同时保障完成结果的队列容量。工作预算分别预留输入、最大输出、临时工作区和完成记录；解码大小尚未知时先有界探测头部，再在大分配前取得输出额度。CPU 输入、解码结果与 GPU 资源各自持有票据，上传过程中的重叠驻留分别计量。WorkBudget 含 max_units:u32、max_bytes:u64、deadline:Option<MonoTime>；WorkProgress 报告实际消耗与是否仍有工作。
- PlatformCaps 含指针、触摸、手柄、文本输入、剪贴板、可访问性、挂起恢复、等待及唤醒支持位。TextInputState 含启用值和逻辑坐标的插入点矩形。输入事件带输入序号、设备身份和事件时间；尺寸事件同时携带物理像素、逻辑尺寸和缩放系数。
- SurfaceInfo 含 physical_size:[u32;2]、scale_factor、surface_epoch、color_encoding 与 alpha_mode；零尺寸代表暂不能呈现。RenderCaps 含支持格式/效果集合、纹理与离屏尺寸上限和上传队列限制。UploadBatch 拥有资源版本、尺寸/stride/格式、像素或网格数据及预算票据；所有乘法和长度校验先于接受。
- PresentOutcome 为 PresentAccepted(FrameId)、Skipped(reason)、SurfaceUnavailable(reason)。RenderEvent 至少有 UploadFinished(id,result)、ResourcesRetired(versions) 和 DeviceLost(error)；可选的实际呈现反馈另带 FrameId 与时间，不能替代资源回收通知。返回的 FrameId 必须对应本次提交帧。
- ReadRequest 含资源身份与版本、offset:u64、len:u32、优先级、Scope 和预算票据；读取结果拥有 bytes，并明确 eof，长度不得超过请求。SaveWrite 含槽位、提交序号、完整槽位数据、最低持久提交级别与预算票据。IoCaps 声明范围读取、读取/保存队列限制及可提供的提交级别；只保证可见替换、不保证持久性的实现不得报告持久保存成功。
- WorkRequest 含 kind、拥有型输入、Scope、优先级及预算票据；kind 限于已注册的解码、编码、校验操作。WorkCaps 含 Threaded/Cooperative、支持种类和队列限制；不支持的 kind 在接受前拒绝。处理进度不算终态。
## 平台与时间
PlatformHost 提供命令，原生事件循环驱动 Player::on_event，不要求平台提供反复阻塞的 pump_events。PlatformEvent 包括输入、组合文本、焦点、尺寸与 DPI、挂起、恢复、内存压力和关闭；输入采用引擎自己的键盘、指针、触摸和手柄数据。剪贴板请求拥有读写参数，以 ClipboardId 关联一次结果；可访问性更新使用稳定节点身份，平台动作从同一事件桥接返回。不支持的能力明确拒绝。平台在组装时给媒体、IO、工作执行器与渲染完成队列注入可克隆的 WakeSignal，后台只用它唤醒循环，不借用整个 PlatformHost。结果入队或 AudioStatus 改变时合并唤醒，即使画面静止也会处理完成消息；纯音频完成保持布局与呈现按需执行。实时供给只使用已验证为有界的通知路径，通用平台唤醒可由控制线程转发。
PlatformCaps 声明输入、文本服务、可访问性及等待/唤醒能力；重绘可合并。MonoTime 使用固定单位的 u64，不保存 std::time::Instant。会话逻辑时钟独立累计，可暂停与恢复；墙钟可缺失，只用于显示日期等。恢复后重设截止时间，系统睡眠不得变成巨大动画增量。
## 渲染与音频
SurfaceInfo 只含尺寸、缩放和引擎颜色约定。RenderCaps 声明纹理/离屏上限、格式和效果支持集；RenderFrame 保留画序、分组、裁剪与颜色/alpha 语义，具体路径和字形转为可适配的几何、掩码或图集。效果以 ID 和纯参数表达，只有 WGSL 实现的效果不能宣称可跨后端使用。
render_present 只在调用内借用帧；后端若保留命令，必须取得数据和资源引用所有权。上传批次包含资源版本与预算票据；接受后由后端持有直到完成或失败。retire 只禁止新引用，实际 GPU 使用结束后才回收。表面不可用或设备丢失须报告并重建，保留会话状态；不能继续使用旧 GPU 身份。
呈现结果区分 PresentAccepted、Skipped、SurfaceUnavailable。PresentAccepted 表示交给呈现系统，不证明扫描显示或 GPU 使用完成。PresentAccepted 更新命中快照；提交成功、实际显示与 GPU 回收分别记录。平台可额外提供实际呈现时间。
AudioRequest 含首选采样率、允许的设备格式/布局与最大供给帧数。AudioCaps 描述实际可协商配置、供给块范围及挂起能力，不将采样率与布局的任意组合都视作有效。configure 只在停止状态调用，返回 AudioFormat：实际 sample_rate_hz、固定 mixer_channels=2、device_sample_format、有序 device_channel_layout、max_pull_frames 与 channel_map。规范混音格式固定为交错 f32 的 L、R 双声道，采样率采用协商后的设备采样率；设备的 8 声道不意味着要求混音器输出 8 声道。Kira Frame 的左右声道与此对应，媒体层在协商后初始化其 Backend 桥接。[14][15]
输出适配器负责从规范混音到设备布局与格式的转换。单声道为 0.5×(L+R)，立体声映射 L/R；多声道默认仅映射到已识别的前左/前右，中心、低频与环绕填零，不擅自扩展声场。未知声道布局必须重新协商已知布局或返回 Unsupported。Windows 8 声道用例核对实际通道顺序、采样率与缓冲长度，不能只检查设备通道数量。
每次 PcmFill 接收 n 帧、恰好 2×n 个 f32，1≤n≤max_pull_frames，切片只在调用期间有效。设备块超过上限时分块供给；设备缓冲长度必须为设备声道数的整数倍。输出适配器预分配转换缓冲，每次先清零供给区，混音器覆盖完整样本；异常长度不得越界，须静音并报告设备契约错误。非有限样本替换为零，转换为整数格式时限幅。回调路径不分配、不读取文件、不等待主线程。
PcmFill 仅拥有混音状态、预分配缓冲与有界控制队列，不捕获 VM 或舞台。start 在接受前完成设备准备；拒绝返回尚未调用过的原 fill 及原因，可在兼容格式下重试；格式变化必须重建 fill。接受后供给由唯一输出线程串行调用，后续设备失败只通过 AudioStatus 报告。AudioStatus 含 Stopped/Running/Suspended/Failed、欠载计数和最近设备错误。stop 无论返回成功或错误，都须先保证不再调用 fill；不得在 fill 内等待 stop。欠载填静音，断连与采样率变化先停止，再配置并重建混音供给；不在后台悄悄更改格式。
音频状态转换按以下规则执行：configure 只用于 Stopped；start 只接受已配置的 Stopped，接受供给后进入 Running。suspend 成功时已等待当前供给结束，进入 Suspended，保留相同 fill/mixer 并冻结逻辑音频时间；resume 验证设备格式一致后复用它。suspend 或 resume 失败进入 Failed，保留旧供给所有权直到控制侧 stop。stop 在任何状态先停止后续供给并等待在途调用结束，再丢弃 fill，返回 Stopped；返回错误仍保证供给已停止。设备断开或格式变化进入 Failed，原因分别标为 DeviceFailure 或 ReconfigureRequired；状态变化触发 WakeSignal，主线程媒体协调器读取状态后安排恢复，实时供给只发布故障、停止调用 fill 或输出静音。此时暂停媒体逻辑时间，通过 stop、configure、新建 Kira manager 和 fill 恢复。媒体服务以稳定 MediaId 保存资源、位置、音量、循环与分组参数，重建原生音轨并重绑已有逻辑活动及等待；增加 MediaEpoch 过滤旧完成消息。成功重建保持活动身份并继续，恢复失败只把受影响活动结算 Failed 一次。重建期间允许短暂停音；普通挂起采用保留 mixer 的路径。原生 Kira 句柄始终留在媒体适配器内部。
### PCM 释放与主循环
音频服务使用有界的非实时清理工作线程处理已接受 PCM 的最终释放。主控制线程先取得清理队列信用，再从 Kira 退休队列移交完整 Sound；队满时对象保持原位，后续维护继续推进。Sound、实际 PCM 与预算票据一起转移，物理释放完成后归还额度并唤醒等待者。接口分别记录已移交数量与已完成释放，申请解码预算通过异步等待完成通知，或返回 Busy 后由完成通知唤醒重试；主线程与实时音频供给持续推进。
每个 mixer generation 创建时预留 Renderer、AudioManager 的一次性退出信用，普通退休任务使用独立额度。成功 play 持有的原 DecodedSound 引用也纳入有界后台移交，覆盖各个可能成为最后引用的路径。创建或重建先取得所需信用，旧 generation 的总数量受限。同步拒绝的请求按原接口返还调用者，由调用者安排其释放。
stop 先关闭供给并等待在途调用结束，再销毁 fill 外壳；外壳与 mixer 将内部混音对象交给清理服务。Stopped 表示供给已停止，PCM 驻留以票据和清理完成记录为准。清理服务由宿主持有，跨越 mixer 寿命；宿主最终退出时显式 drain/join。验收分别测主侧移交、后台实际释放、原生 pause 与供给栅栏等待，0.5ms 主循环回收目标针对主侧工作。所有队列、在途清理对象与保留的旧 generation 都计入容量和驻留边界。

## 资源与工作执行
ReadRequest 使用 AssetKey、版本、offset:u64、受限 len:u32 和优先级，不要求路径、mmap 或文件监视器。AssetIo 可读包、内存或平台文件系统，接口须快速返回；真正可能阻塞的文件调用交给平台异步机制或 IO 工作者。SaveWrite 使用应用保存命名空间和独立进程请求身份；完成只表示该平台已验证的持久提交级别。缺少可靠原子替换的平台须采用经故障验证的双槽或版本日志，不能假定 rename/fsync 一定适用。
WorkRequest 是图像解码、快照编码、包验证等有限工作描述，拥有输入、优先级、取消身份与预算票据，不接收脚本闭包。WorkCaps 声明线程或协作模式及支持种类。协作实现保留游标，在工作单位、字节或截止时间到达时让出；不能用一次阻塞整图解码冒充分片执行。无工作线程且无可分片实现时，明确不支持该工作或要求预转换资源。实时音频供给不能被普通协作任务饿死。
所有队列同时限制数量和字节。拒绝返回原请求及原因，不丢数据、不暗中改为同步执行。完成消息拥有自身数据，包含请求身份、所有者或会话代数，最多一个终态。业务取消先撤销对应等待/订阅，再请求底层取消；结果路由始终核对等待仍有效及 Scope 匹配。服务尚未结算时取消产生唯一 Cancelled；已结算且排队的终态保持原结果，交付后只用于额度与进程账本结算，已取消业务保持终止。这样已发生的保存提交仍有准确记录，旧读取或工作结果也无法重新唤醒已取消任务。poll/drain 每次最多写入 limit 个事件，调用方预分配容量。IO 读取与保存各自保留容量；完成消息通过两路轮转等有限等待策略交付，持续读取也须让已完成的保存及时出队。
## 桌面与 PS Vita 目标
首批桌面目标是 Windows、Linux、macOS，各组合完成构建和行为验证后才列为支持。移动端与 Web 需要另外适配生命周期、输入、媒体与存储。PS Vita 是未来移植目标，当前接口不代表已经可运行。
Rust 的 armv7-sony-vita-newlibeabihf 目标为 Tier 3，依赖 VitaSDK，标准库部分支持且无预编译标准库；VitaSDK 是开源 homebrew 工具链。[10] 因此先验证锁定工具链和完整依赖树，不从 target 存在推导 reel-ui、Parley、Kira 等已兼容。
Vita 的 GXM 有专门纹理布局、内存映射与同步要求，GPU 预算需按不同内存区域实测。[11][12] 音频提交可能阻塞，应由专用路径处理，输出适配器按实际端口把 f32 转换为所需 PCM，转换缓冲预分配。[13] 32 位构建不保存指针或 usize 布局；长度转换、stride×height 与对象索引做溢出检查，不假定 AtomicU64 可用。桌面 N100 的帧时间和资源预算不直接用于 Vita。

## 四 线程与帧调度
主线程拥有窗口、舞台、UI 和 GPU 提交。VM 在主线程按预算执行，避免细粒度跨线程调用。默认桌面把读取、解码和存档编码交给工作线程，其他平台依所选 IO 与执行器契约推进；音频由独立输出路径保障。后台队列同时限制任务数和数据字节数，当前可见资源优先于预取。
每帧固定执行：输入分发；预算内脚本与后台完成消息；动画采样；状态提交；必要布局与排版；命中几何与绘制计划更新；呈现。布局属性动画参加本帧布局，纯变换与透明度只影响合成和命中。静止画面等待输入、计时器或资源事件，活动动画才持续请求新帧。
输入采用最后一次被呈现系统接受的帧几何，收到 PresentAccepted 才替换快照。单次输入固定使用同一份快照；获取表面、提交或呈现失败、过期，以及 Skipped 或表面不可用时都保留原快照。该规则不承诺屏幕已经扫描显示。命中后核验当前节点代际与交互有效性，已销毁节点不再接收事件。一次输入最多推进一次剧情。
reel-ui 的交互分发层是唯一控件输入入口。它使用冻结的 PresentedGeometryView 进行定向分发，该视图包含 NodeId/WidgetId 映射、局部尺寸、逆变换、裁剪、层序和祖先路径。按下、释放、hover、捕获坐标及 EventCtx 的几何查询都读取本次输入选定的视图，保持同一依据。定向事件入口按冻结几何分发，禁止随后又按 widget_arena 的当前布局二次命中。已销毁或禁用节点由当前有效性检查过滤。控件先处理并消费输入，剩余推进事件才进入剧情。按钮配对按下和释放，滑条捕获拖动指针。模态容器隔离下方指针、焦点、键盘和剧情快捷键，并释放作用域外的捕获与快进保持；失焦清除捕获与按键保持。

## 五 统一舞台与合成
StageGraph 唯一拥有作者可见节点、稳定 NodeId、父子结构、声明属性与寿命。reel-ui 的控件模块负责对应控件的布局、焦点、捕获、输入法、可访问性和临时交互状态；复合控件内部节点保持私有。两者通过 NodeId 到 WidgetId 映射连接。业务值和控件瞬态不得成为两份互相覆盖的状态。
脚本变更与控件动作进入有序 StageTransaction，支持创建、移除、挂载、脱离、替换场景、属性修改、绑定和订阅。提交前统一验证身份、单父与无环约束；失败保留原舞台，成功后才释放旧场景。属性存储是唯一权威，同一事务同一属性采用最后一次有效写入，范围约束先执行，随后静默更新投影。属性绑定保留被引用属性的读写权限，slider 写回需要可写属性；只读派生绑定跟踪实际读取的可观察属性；派生函数不得写入或等待，循环依赖给出源码诊断。
节点内容与交互行为分开。图片可以具备按钮行为，容器可以整体变换；按钮外观读取 hover、pressed、focused、disabled 等状态，滑条提供轨道与滑块槽位。作者组件与内建组件使用同一舞台能力，具体调用形式见 DSL 文档。节点移除注销绑定、订阅并取消所属任务与活动；隐藏保留身份，脱离必须由显式所有者接管。visible=false 隐藏子树并退出命中与焦点；enabled=false 保留画面并禁用子树交互。两者使捕获失效时统一释放捕获；opacity=0 不自动禁用输入。
仿射变换、裁剪和命中使用同一几何，命中经逆变换计算。非线性视觉变形默认采用作者指定的逻辑命中区，扩展映射另行提供。适配必须验证变换后控件的捕获、焦点与命中一致性。
便携的 RenderFrame 与绘制命令定义于 reel-ui::frame，帧资源由 reel-ui::resources 管理，基础资源身份定义于 reel-core；UI 从不反向依赖 render。Mesh、Material 等扩展类型按既定功能范围逐步实现。RenderFrame 包含帧号、画布与输出尺寸、资源引用以及有层次的绘制计划。计划保留 BeginGroup、EndGroup、裁剪、组透明度和效果参数。组内绘制连续，子节点排序受组边界约束；跨组覆盖由显式 overlay 容器表达。组透明度对合成结果应用一次，效果外扩计入离屏范围，再遵守对应裁剪语义。
普通容器直接合成，遮罩与后处理按需使用离屏目标。先实现相邻兼容绘制合批、排版与字形缓存、离屏目标复用；逐字显示复用完整文本排版，只改变显现范围。属性变化区分布局、内容、合成和命中；淡化不使排版失效，资源版本变化传播到依赖缓存。GPU 资源在实际使用结束后释放。低端模式可降低效果分辨率和采样数，逻辑坐标保持一致。

可写绑定的标量写入与 tween 均沿引用链写入最终源 PropertyId，保留绑定。run 解析并保留真实源身份；被动投影只读取结果，外部源写入按统一规则停止根活动。只读派生属性拒绝直接写入，可用独立 override 层作演出。binding replacement/unbind 先停止依赖旧链的活动；unbind 固定当前基础值。override token 拥有独立属性存储，也遵循读写能力与保留规则。

## 六 作用域 样式与状态
scope 是普通宿主 API，通过 VM 的 CallScript/native continuation 在原 Task 中执行 body。每个作用域有 RegionId 和清理记录，管理显式加入的节点、覆盖层、偏移和活动。正常返回/异常同步幂等清理，退场动画由 body 显式等待。作用域被附加 owner 撤销时，VM 只撤销到 RegionId 屏障的区域，宿主先清理，再于外层 scope 调用点交付 ScopeCancelled；外层主剧情仍可处理。硬撤销跳过区域内脚本 finally，普通 return/throw 仍按语言规则展开。
属性覆盖是独立 token，记录目标、字段、优先序和值或绑定。基础属性与绑定继续更新，求有效值时取最后建立的有效覆盖；移除某层只显露剩余层或最新基础值。活动写入明确的基础位置或覆盖 token，effective 值只是解析结果。覆盖字段的直接写入与动画保留遵守统一冲突规则。
s.offset(node) 创建父坐标系中独立的 x/y 加性偏移层；各层相加后进入节点最终变换和命中矩阵，原布局值保留。旋转/缩放采用有序矩阵和组节点，避免把非交换变换伪装成加法。偏移与覆盖均可进入快照，失效时释放命中及绘制依赖。s.run 始终归 scope；run 默认归当前 scope，否则归 Task 的 owner，显式 owner 表示明确延长生存范围。目标失效仍按根活动取消规则处理。
样式复用由普通组件函数、记录与参数完成。组件默认值、变体、显式参数按序合并；显式缺失与 null/false/0 区分，scope 覆盖最高。运行期样式通过可观察属性绑定，布局/绘制失效分类保持一致。引擎提供通用属性和控件槽位，作者决定视觉风格。
model(defaults) 复制默认图并保持其中的别名/环，所有可变内嵌数据对象使用观察变更钩子，读取与结构操作记录对应依赖；后续会话对象赋值保留共享身份并纳入观察，别名修改同样通知。原输入默认图保持独立。对象按可达性保活。state(id,defaults) 是会话内的具名状态根，随保存/回滚恢复，新游戏初始化。node.state(key,defaults) 属于具体节点实例的状态槽，在节点 key 与槽 key 同时匹配时重载复用；同名不同实例互相独立。组件销毁后保留的普通脚本引用按正常 GC 规则决定对象是否继续存活。
persistent(id,defaults) 是显式进程级数据根，跨游戏会话存储；defaults 仅补缺失项，已有 false/null/0 保持原值。新实例默认复合对象复制后归该实例，避免共享可变默认状态。持久数据由独立数据图与适配器保存，禁止把会话 Node/Task/可执行闭包挂入跨局数据图；写入时检查引用路径与寿命。跨局赋值复制到持久数据图，内部嵌套对象通过稳定 GlobalDataId 的代理访问；所有别名修改均验证并标脏，快照记录 GlobalDataId 并重接当前数据。全局设置属于独立服务。
普通运行期 state/persistent/node.state 调用原子补充缺失默认字段，下一语句可见。InitSafe 的 story.register、state、persistent 及组件状态默认补充先登记候选变更，候选读视图可见拟加入字段而活会话不可见，ModuleSet 提交时才一起生效；失败不修改旧注册表或全局进度。全局数据的普通修改立即可观察、后台合并写盘；flush 返回可等待的可靠提交请求。跨结构版本迁移使用版本化纯数据回调，在隔离数据图成功后再替换。

## 七 VM 与会话接口
会话管理主剧情、UI 回调任务、交互数据和活动所有权。启动与主剧情任务归会话所有，UI 回调归发出事件的节点所有；替换场景结束旧节点及其活动，主剧情任务继续。主剧情同一时刻只有一个交互。game 服务身份稳定，game.dialogue 属于当前会话，settings.music 属于全局设置。发言更新完整文本与显现范围并追加一次历史，再等待推进；文字未显示完时首次推进只完成文字，后续推进才结束等待。choose 将输入普通表复制成引擎拥有的只读可观察选项行，id、key 与 label 均可读，id 是 InteractionId 加 key 的不透明稳定标识，列表用 id 区分连续选择；select 接收带 InteractionId 的 option.id，验证后返回业务 key。入口与剧情 helper 均为普通 fn，剧情元数据按需附加；具体语法见语言与 DSL 文档。
VM 的 run_slice 接收指令、分配和计时检查额度，返回完成、预算让出、宿主调用、等待或错误。宿主调用携带任务身份、参数和源码位置，返回 Ready、Pending、CallScript、Continue 或 ScriptError。Continue 保存 native 算法状态并于同 Task 下一预算片继续，无外部等待。CallScript 保存纯数据 native continuation，退出原生栈后在同一 Task 压入脚本回调帧；回调可按其效果上下文挂起，返回/异常恢复 continuation。Pending 先退出 Rust 调用，再建立显式恢复记录；恢复记录保留操作、可恢复参数、返回目的槽与下一指令位置。禁止把原生调用栈当作可保存续体。同步原生重入 VM 不得跨边界挂起，异步宿主算法使用显式状态机。
SessionEpoch 在新游戏、读档和回滚时更新；任务、等待、节点、回调和活动各用独立代际身份。后台完成结果只有与仍存活的 SessionId/Epoch 及等待匹配时才交付一次。取消、节点销毁和会话替换使旧结果失效。
ActivityHandle 本身实现 Awaitable，支持暂停、继续、停止和到终值完成。stop 保留当前值返回 Stopped；finish 对有限活动执行到终态返回 Completed；无限重复没有完成终点，由 stop 或 owner 结束。终态为 Completed、Stopped、Cancelled、Failed，只提交一次；前两者返回相应结果，后两者在等待位置产生取消或脚本错误。所有者取消任务时直接终止该等待。所有 VM 任务遵循同一可等待对象契约，恢复身份进入快照。主剧情输入权限仍由会话管理，与函数声明形式分离。
演出描述先纯构造，再由 run 在显式所有者下启动；顺序组合不接收已运行句柄。动作激活时读取起值。run 先计算完整动作树的属性写集合，包括 sequence 后续段、repeat 和暂停阶段，并原子保留到根活动终止。sequence 内重复属性按顺序使用；parallel 各分支的写集合必须互斥。保留冲突时保持已有活动并在 run 处报错。直接给已保留的属性赋值时先停止对应根活动，再采用赋值，即使该属性的分段尚未激活也遵守这一规则。显式 stop 使根活动及余下子动作返回 Stopped；任一子动作 Failed 则结束整组并返回错误。节点与效果目标以非拥有型代际句柄引用，run 校验目标有效；任一目标销毁使整个根活动 Cancelled，释放全部保留并向仍存活的等待者交付取消错误。所有终态只提交一次，均释放保留。持久活动图包含子活动、进度与等待关系，并提供可序列化恢复描述。描述中不保存闭包，事件回调排入普通任务。接口见 DSL 文档。
已读标记按稳定语句 ID 记录。按住快进由输入保持状态驱动，松开、失焦、进入菜单和读档时清除；自动播放属于会话状态，文字速度和音量属于全局设置。更换 UI 不改变历史。

## 八 资源与音频
资源以稳定 AssetKey 和内容版本管理读取、解码、上传与缓存。取消预取、场景退出与过期结果都释放所属额度。图片、字形、视频队列、GPU 纹理和临时目标分别记账；缓存只保留可逐出的额外引用。直接资源引用与作者预加载共同提供预取提示，动态分支确定后调整集合。
默认音频用 Kira 适配音乐、语音、音效与 UI 分组；各目标输出桥接单独验证。脚本发送控制，实时供给不得执行文件读取和 VM 工作。控制队列有界，连续音量更新可以合并；播放补给拥有独立工作容量与队列额度，图像解码不得耗尽它。短音效预解码，长音轨采用流式策略。[9]
预分配回调路径、锁竞争、声道与采样率协商必须实测。Windows 8 声道设备配置列为重点用例。控制队列为停止、设备关闭与音轨释放预留独立容量；播放请求进入普通有界队列，满载时返回带原请求的 Busy。音量按音轨合并为最新值。停止请求按音轨幂等合并，关闭状态优先于新播放，保证回调线程始终有界且主线程获得明确结果。具体容量由目标配置给出，满载场景进入契约验证。
视频采用 reel-media 内的 FFmpeg 适配器，复用 ffmpeg-next 的流式 IO 接口连接 AssetIo；包装库和原生 FFmpeg 作为固定、验证过的组合发布。音频帧交给 Kira，视频帧按 PTS 送渲染器，音频可用时以音频逻辑时钟为主，否则使用会话时钟。解码、seek、队列和上传有独立预算与媒体代数；落后帧按播放策略丢弃，结束只结算一次。透明视频支持配套遮罩轨或 side-mask 布局，和普通节点一样进入合成。各平台分别验证静态/动态链接、编码许可及硬件解码；video feature 控制是否装入这项完整能力。

## 九 存档与回滚
存档在所有可修改持久状态的任务停在合法指令或等待边界，且舞台、绑定和资源切换已提交后捕获。保存、读档提交与热重载提交互斥。保存接口立即返回请求，进度由服务模型提供；发起保存的任务返回或停在普通可恢复指令/等待边界后即可捕获，因而允许等待自己的 SaveRequest。当前存档副本内将该请求及其 Future 记为 Succeeded；活会话保持实际写入状态，只有可靠提交完成才成功。读取这份已提交存档时，恢复请求从已保存的后继指令继续，等待者在下一调度步按登记顺序收到成功，保存调用执行一次。此规范化只作用于被持久提交的该存档副本，既不修改活会话，也不适用于内存回滚快照。已结算的进程请求直接恢复保存的终态与结果。其他尚未结算的进程写请求只有匹配精确进程和请求身份及完成记录才重接，否则结算 OperationInterrupted 并保留可处理错误，避免重新写入外部目标。
快照包含引用模块版本、全部存活任务及其调用帧与恢复位置、返回槽、异常状态、寄存器与中间值、闭包和共享 cell、全局对象图、待交付结果、当前交互、组件数据、舞台逻辑状态、时钟、随机数及媒体描述。generator、thread 与其他可等待对象按语言定义保存。对象 ID 保留共享、环、弱引用和类关系；脚本回调及捕获进入同一对象图。
设备句柄通过资源描述重建。每种可达宿主值必须有版本化恢复描述；缺失时报告对象路径，保存失败且保留原槽位。系统服务恢复为同一服务身份。读取在隔离候选会话中先创建身份、再连接引用与订阅，成功后一次切换；失败保留当前会话。
捕获先固定会话时钟、根与媒体逻辑状态，再暂停业务写入，按对象与边游标分片复制到独立快照。长字符串与容器也应分块。窗口、输入采集和 UI 的非持久原生反馈继续运行；业务回调与有副作用的绑定写回按序排队；derive 失效与只反映最终瞬态的外观重算可合并，完成捕获后按最新状态重新计算。后台结果排队。连续移动按指针合并，按下、释放、取消保留次序。队列接近容量时提前终止本次捕获并恢复业务处理，保存请求报告 CaptureBusy，原存档保留；仍发生输入溢出则发布 InputReset，释放全部捕获与按住状态并丢弃该溢出批次，避免卡住按钮或快进。队列内事件保留会话和节点身份，恢复后重新验证。后台只接收已完成的纯数据快照，负责压缩与写盘。
资源读取按描述重发，动画按逻辑时间继续，音乐按资源和位置恢复，当前语音从该句开头重播，一次性音效保持已结束。外部写入使用结果记录与回滚边界，不能随读档盲目重放。
槽位文件统一保存摘要、缩略图和完整快照，桌面默认在必要同步后原子替换，其他平台遵守 AssetIo 已验证的等价持久提交策略；同槽请求按序提交，只有对应存储级别提交成功才报告完成。失败保留旧文件。捕获后的写盘由进程级保存服务持有，切场景不取消它。平台同步与替换行为必须实际验证。自动保存可合并，显式请求保留顺序；待捕获请求数量与估算字节数有上限。
全局设置、已读和解锁记录独立于剧情快照。恢复设置绑定时以当前全局设置为准。回滚先使用完整交互快照和共享不可变资源，默认历史预算起点为 32 MiB，超限移除最旧快照；活动 cell、属性和等待状态必须独立复制。增量记录等真实成本明确后再做。
每个闭包和挂起调用绑定确定代码版本；旧模块保留自己的全局环境与导入绑定，最后引用退出才释放。发行存档由匹配游戏包提供代码；开发存档嵌入无法从匹配固定包取得的全部所需模块，包括活动版本。缺失版本进入明确迁移流程，不自动改用新代码。

## 十 热重载
开发工具合并变更事件，编译并验证候选模块；失败显示源码诊断，继续运行原版本。新版本只在安全点提交。资源也先准备新版本，再替换引用。[8]
发布更改模块及其全部反向依赖的新 ModuleSet，固定各实例的导入绑定。候选初始化只可修改候选对象，宿主副作用要求 InitSafe；失败保留旧会话与全局设置。普通调用及当前剧情继续使用原模块集合。game.start 接收模块级零必需参数函数的稳定符号身份，独立于可选的 story 注册表，在替换会话前于最新已发布集合验证符号与签名，然后创建全新的模块全局。game.restart(story_id) 使用当前会话的主剧情模块集合并保留其变量。显式组件重建可采用新集合和调用者传入的旧模型；组件私有全局采用新实例初始化值。组件依稳定 key 和局部数据重建，替换订阅后释放旧订阅。拖动中更新推迟至手势结束或明确取消，业务状态与控件瞬态分别处理。
切换当前剧情属于显式开发操作，按命名段落与交互点检查状态结构；从段落入口重跑必须提示会重新执行该段动作。普通读档和回滚始终恢复完整快照。候选组件构造沿用初始化副作用限制：可分配候选节点和模型，只读已有业务对象；修改现有业务对象或启动外部操作会在构造处报错。候选模块与舞台替换完整准备后统一提交，失败保留旧版本。开发热重载不承担任意旧发行存档的自动迁移。

## 十一 完整制作能力与扩展
最终能力以 Ren’Py 的制作能力覆盖并进一步提升组合自由度为目标。以下能力均进入完整配置的实现与验收；构建 feature 用于发行裁剪。每项必须有真实接口、对应实现和作者用例，统一节点或扩展接口的存在本身只是承载机制。
剧情阅读：ADV、NVL、气泡及多角色发言共用一个 DialogueInteraction。character.line 创建纯对白数据，say(lines, mode:, append:) 把一组行提交到 game.dialogue.lines 并共享一次推进；character 的普通 _call 是单行快捷调用。每行有独立角色、文本、揭示范围、语音与位置，NVL 可累积到显式清页。历史记录保存行组与内容身份。文本尚未揭示完时推进先完成本组，随后推进结束等待。回放使用隔离会话，退出恢复暂停的原会话；全局持久写入默认只读，作者需显式选择允许。
UI 交互：按钮、图片按钮、滑条、输入框、滚动、虚拟列表/网格、热点、拖放、工具提示、快捷键与模态。拖放有 DragId，记录源、载荷、捕获、候选目标与接受条件；按下/移动/释放/取消分别交付一次，失焦或 owner 撤销转 Cancelled，落点以同一呈现几何计算。文本输入区分编辑值、选区与 IME 组合串，组合期间保持平台候选框定位。组件公开语义角色、名称、值、操作与焦点顺序。
文字：文本服务接入 Parley 和字体回退，统一布局普通文本、样式 span、内嵌图片、链接、ruby 与双向/CJK 文本。富文本解析产生有源码位置的结构 span，文本标签/处理器可由作者注册。reveal 按字素映射到完整布局 cluster，速度/暂停标记属于交互时间线，快速完成保留最终排版。文字效果取得字形/cluster 标识与纯参数，复用 Effect 管线。
本地化：LocalText 保存稳定内容 ID、原文与命名参数，构建工具维护提取索引和翻译目录；作者可显式指定 ID，自动 ID 的编辑映射保留在项目中。目录含 locale、fallback 链、文本及资源替代；语言变化使相关文本/字体/资源缓存失效。历史保存内容 ID、参数、原文与已显示结果；默认按当前语言重新解析，缺译显示原文。提取、更新、缺译和参数不一致通过 reel loc 命令诊断。
可访问性：统一语义树驱动键盘/手柄导航、屏幕阅读器、TTS 与剪贴板辅助输出。作者设置替代文本、分组和朗读顺序；TTS 朗读当前已提交交互，与语音策略和自动推进服务协调。字号、对比和揭示速度可作为设置模型绑定，布局按实际文本尺寸更新。
图像与二维演出：分层立绘由条件资源子节点与共享参数组成；变换、遮罩、颜色矩阵、曲线、顺序/并行动作和粒子/精灵节点共用活动协议。Transition 持有 old/new 两个已冻结的子树输入及进度；成功完成后只留下新树，取消按声明策略保留旧或新一侧，释放只属于过渡的离屏资源。差分选择与动态资源版本都进入保存状态。
透视与可编程渲染：场景组可带 Camera 与 3D Transform，RenderFrame 支持版本化 Mesh、Material、纹理、uniform、混合/深度设置及效果输入。普通节点生成二维平面网格，透视后仍参与统一组顺序；显式深度组使用自己的深度缓冲规则。交互几何可以是二维逆变换、投影平面射线映射或注册的自定义映射。着色器改变形状时作者提供匹配命中策略。WGSL 是默认后端实现语言，Effect/Material 身份和参数独立于后端。
作者自定义节点：custom_node 注册可保存 state、measure(constraints)、record(draw_context)、event(event_context) 与显式重绘请求。绘制回调记录有资源身份的绘制命令，布局与绘制阶段只读并保持有界；异步加载由正常任务完成后标脏。作者可以增加小游戏、图形和输入行为而沿用同一布局、变换、焦点与恢复协议。Live2D 通过格式/SDK 适配节点提供模型、动作、表情及可恢复参数，SDK 许可与目标构建随适配器明确列出。
音频视频：音轨可排队、定区间循环、淡化、声像、滤镜与同步组起播；语音支持逐句关联和历史重播。各操作对应可恢复媒体描述，队列/滤镜状态与逻辑位置进入快照。视频支持普通节点、全屏、循环、跳过、seek、音画同步与遮罩。错误、设备恢复和取消遵守媒体身份与唯一终态。
存档与历史控制：普通/快速/自动槽位、回滚、前滚、选择锁定、回放和持久解锁共用服务。前滚只复用仍匹配代码/状态版本的已记录交互与选择；分支改变使后续记录失效。作者可建立显式回滚边界。发行升级保存代码版本清单，构建工具保留所需历史编译产物；迁移在隔离候选中验证后切换，运行位置只通过声明的恢复映射改变。
平台与发行：桌面 Windows/Linux/macOS、Android/iOS/Web 以及额外 PS Vita 目标分别实现窗口、输入、音频、存储与生命周期适配。发布清单记录能力与后备策略，实际支持按平台验证。移动签名与 SDK、商店服务和受限平台编码能力由对应构建适配器处理。
外部服务：文件、HTTP、下载、更新、成就和平台购买使用版本化宿主服务。读取可按描述重发；外部写入带请求身份、幂等键及结果记录，保存/回滚遵守已定义的进程写恢复规则。native 模块默认静态注册，支持动态链接的平台可用版本化 C ABI；句柄为不透明 ID，资源与错误均有明确释放规则。生产发行能力与权限在构建时声明。
工具：reel check/dev/build/loc/test 提供诊断、资源检查、调试与构建。节点、样式、属性绑定、资源、活动及任务可检查；工具模式的表达式默认纯读。测试驱动输入、截图及行为结果可自动对照，演出编辑辅助输出可维护源码。语言 LSP 与 debugger 使用同一签名和源映射元数据。

## 十二 字节码与资源分包发布
发布产物分为字节码包 .asb 与资源包 .rpk，随目标平台播放器和 release.json 清单发布。一个游戏可以有多个代码模块包与多个资源分包；脚本修订和图片/音频更新独立构建。此分层借鉴 YU-RIS 的脚本二进制与资源归档组织，格式由 Reel 自行定义。
.asb 保存入口、模块/函数/导入导出、常量、稳定内容与恢复标识、语言/字节码/宿主 ABI 版本和所需能力。源码映射可作为独立调试包，去掉源码后仍运行完整字节码语义。.rpk 保存 AssetKey、内容版本、目录索引、块表、编码/压缩信息和数据；索引与 offset/len 使用固定宽度，range read 支持流式音视频。已压缩媒体可原样储存，通用数据按独立 zstd frame 压缩，避免为一个 seek 解压整包。
release.json 使用 UTF-8 JSON，列出 release_id、入口、引擎/格式范围、必须能力、代码包与资源包的路径、大小和 BLAKE3 内容摘要。代码与资源头部各有魔数、major/minor、索引位置与长度；加载先检查清单及索引，再按需校验数据块。路径规范化到包命名空间，长度/偏移与解码预算在访问前验证。包格式不依赖 Rust struct 的内存布局。
构建从干净输出目录生成产物，清单最后提交；更新先下载到候选目录并验证匹配关系，再原子切换活动 release 指针。失败保留原 release，用户存档、设置与 persistent 位于独立用户数据目录。代码包保留精确模块版本，资源引用带内容版本，防止静默混用旧代码与新索引。可选发行签名通过显式签名步骤完成，清单摘要用于完整性和缓存身份。
发布验收包括：移除源码独立启动、只更换代码包、只更换资源包、范围读取媒体、损坏/错版本拒绝、更新中断恢复及旧存档恢复/迁移。各平台分包和更新使用同一逻辑协议及其已验证的存储提交能力。

跨模块数据与恢复契约
对白模型以 DialogueInteraction{id,mode,current_line_ids,lines,page_locale} 为权威。lines 的元素是 DialogueLine{id,speaker_id,speaker_name,content,resolved_text,reveal,voice,position}，content 保留内容 ID/参数，resolved_text 是提交时的文本结构。NVL 的已累积行与本次 current_line_ids 分开；speaker_name/text/reveal 的快捷字段只投影第一个当前行，空组投影为空文本和零揭示数。HistoryGroup{group_id,interaction_id,lines,resume_point} 记录一个提交行组；单行快捷字段投影本组第一行。新组或清页均通过一次事务提交，推进作用于当前组，历史添加一次。

音频时钟与媒体读取
AudioCaps 声明输出时钟能力。AudioClockObservation 包含 OutputEpoch、generated_frames、submitted_frames、estimated_presented_frames、对应 MonoTime、sample_rate_hz、latency_frames、uncertainty_frames 及 Measured/Estimated/Unavailable 精度。generated_frames 仅作当前 epoch 的软件生成诊断；成功提交与设备呈现分别记录。Unavailable 的提交/呈现数字为无效占位，查询返回 None。当前 CPAL 缺少逐块提交确认，因此声明 Unavailable；设备时间戳与回调次数仅保留为诊断。原生 Xrun 关闭供给、使 epoch 与媒体映射失效并交控制侧重建；内容队列短缺填零按当前设备时钟协议处理。适配器使用设备时间戳或明确延迟估计，而非把写进缓冲的帧直接当成已经播放。每个媒体同步组记录实际有效 PCM 的设备帧区间到 MediaPTS 的映射，效果/重采样延迟计入映射。欠载填零时设备帧推进，但该流内容时钟停在最后有效内容，视频跟随该时钟暂停；其他音轨可继续。输出 epoch 改变使旧时钟观察失效并重新建立关联。
解码 EOF 表示输入耗尽，播放完成还需最后有效音频区间按观察精度消费、视频末帧已由调度器处理（提交或按隐藏/过期/表面不可用策略丢弃），且媒体时间线到达终点。seek 增加 MediaEpoch，唤醒并取消阻塞读取，清空该媒体的解码/PCM/纹理等待队列，从目标位置建立新映射；旧结果只释放资源。clock_observation 是一致的只读快照，状态变化通过 WakeSignal 通知。
DescribeRequest 按 AssetKey 与固定内容版本返回 AssetInfo{length:Option<u64>,range_read,seekable,version,media_type}；.rpk 静态资源有确定长度。媒体适配定义 worker-owned MediaSource 的 Read/Seek 视图，经独立 IO 队列桥接 core::AssetIo，阻塞仅发生于解码工作线程。取消、关闭和错误唤醒等待者，IO 执行容量独立于等待它的解码工作容量；主线程、音频供给和同一个被占满的工作池不等待这个桥。未知长度或不可 seek 的源采用对应解码能力，调用 seek 前校验能力。

脚本自定义节点
Aside 可直接注册 custom_node 的 state、measure、record、route 与 event 回调。measure/record 及 route 使用明确的受限上下文；route 同步返回 Consumed/Pass、捕获与焦点请求及是否安排业务事件。事件阶段先应用这份有界路由结果，再把 event 作为节点 owner 下的普通可暂停任务排队。route 超预算或失败时消费当前输入、撤销相关捕获并报告源码错误，避免误推进剧情。纯绘制节点默认 Pass；声明了交互的节点按声明事件范围消费，作者可用 route 细化。record 只记录受预算的绘制命令，异步资源请求由普通任务执行，完成后标脏。这样普通自定义绘制和交互均可用 Aside，原生扩展用于新的底层能力。

透视绘制与命中
逻辑坐标 x 向右、y 向下，z 正向远离观察者；二维单位为逻辑像素。矩阵使用列向量，world=parent×translation×rotation×scale，存储顺序为列主序。相机定义 view 与 orthographic/perspective 投影，perspective 使用弧度 fov_y、aspect、正 near 与 far>near。引擎裁剪空间 x/y 在 [-w,w]、z 在 [0,w]，背后或越过近远裁剪面的片段剔除；各后端转换到本机约定。纹理 UV 原点在左上，颜色在合成前转线性、采用预乘 alpha。
普通组依树与显式层序绘制并反序命中。depth group 以自身深度缓冲组织不透明深度写入；透明项默认保留声明顺序并只测试已有深度，作者显式选择写深度时相应进入深度策略。PresentedGeometryView 保存相同 Camera、投影、网格、裁剪与材质深度策略：普通组按画序，深度写入几何按最近有效射线交点，透明候选再按其画序与不透明遮挡筛选。默认命中使用逻辑几何；透明像素穿透需要声明静态 alpha-mask 或自定义映射。奇异/非有限变换不进入命中；会改变顶点形状的自定义着色器必须给出对应逻辑映射。渲染与命中共享版本快照，后端自行换算不会改变逻辑选择。

文本与语言切换
文本输入分为 PlainText、RichText、LocalText。普通 string 默认 PlainText；RichText 由显式富文本模板或结构 span 构成。LocalText 保存稳定 ID、fallback、参数与模板类型。先解析所选模板的标记结构和占位符，再把参数按声明格式作为文字插入；参数默认只接受可格式化数据，插入内容保持文字而非新的标记。显式结构 span 参数才携带样式/链接语义。标记转义、插值错误与处理器输出均带源码范围。
文本服务先形成逻辑字素序列，再完成 shaping/cluster 映射；revealed_count 指逻辑字素边界，完整 cluster 按该映射显示，ruby/内嵌对象的揭示作为声明的原子单位。当前活动对白组保持提交时的文本和语音，下一组使用新语言；NVL 页保持 page_locale 直到清页。UI 与历史随设置即时重解析。显式刷新当前对白会创建新的解析版本、重新开始揭示并按新语音重播，旧完成消息失效。

朗读服务
TTS 是独立平台服务，接口提供 speak(UtteranceRequest)、cancel(UtteranceId) 和带唯一终态的事件；请求含 SessionId/Epoch、InteractionId、locale、文字/语义片段、owner 与策略。系统语音可直接发声，也可由适配器产出 PCM 接入混音，清单声明路径。SelfVoicing、ExternalScreenReader 与 CharacterVoice 的策略明确选择或 duck：默认角色语音优先，同一内容避免重复朗读；新交互取消旧 utterance。完成事件只有仍匹配当前交互才参与自动推进；取消/失败解除相应等待而不产生重复推进。语义树更新与 TTS 播放是两条独立接口。

回放会话栈
Runtime 按 SessionId/Epoch 路由结果，活动会话只是其中一个。回放把原会话标为 Parked，暂停任务及逻辑/媒体时钟并隔离其输入；仍保留强根和已经收到的有界结果。未完成的可重发读取取消，保存可重发描述；恢复原会话时重新登记请求并原子替换 continuation 的 WaitId。旧读取完成仅用于记账释放。进程写入继续在独立结果账本结算，恢复会话再交付；媒体以已保存逻辑位置恢复。回放使用新的 SessionId/Epoch，默认禁止跨局和外部写入，显式策略才能开放。退出回放撤销其全部 owner，恢复原会话并清除按键保持；每个会话仍只有一个主剧情交互。

资源更新与副作用
作者源码和 .asb 使用稳定 AssetKey；发布清单把它映射为确定内容版本与平台变体；实际打开的资源和快照记录已选版本。资源单独更新只改变新 release 的映射，活会话与在途读取继续持有原 release 引用，相关包直到最后使用者退出才可回收。旧快照需要的版本由保留包提供，缺失时走声明的迁移或版本错误，保持当前会话。开发热替换是显式新资源版本提交，不修改旧快照的含义。
宿主副作用分为可重发读取、服务端保证幂等的写入、不可重放写入。写入在独立于剧情快照的持久操作账本记录 operation_key、请求和状态；重试保持同一业务键，仅在服务契约保证时自动重试。不可重放操作在派发前建立回滚边界；恢复更早状态先核对账本，产生冲突时要求明确恢复策略而非重发。连接断开等不确定结果进入 OutcomeUnknown，通过查询/核实结算；取消等待不等于外部操作撤销。回放默认只读，精确一次承诺限定于已验证的服务与请求协议。

## 十三 性能目标与可观测性
固定 720p、1080p 验证场景记录冷启动、包加载、输入响应、CPU/GPU 帧时间、分配、内存与纹理峰值。首阶段以 Windows x64 作低端体验基准，Linux 与 macOS 各自验证窗口、输入、渲染、音频和持久化。低端参考档为 N100 级集成显卡、8 GiB 内存、720p；每份结果注明实际硬件与构建配置。
目标为控件可见响应 p95 小于 50ms，基础动画 CPU 帧工作 p95 小于 8ms，GPU p95 小于 12ms，呈现间隔 p99 小于 33.3ms。60Hz 稳态同时要求间隔中位数小于 17.5ms、超过 25ms 的比例低于 1%。这些均待实测验证。
主线程起始调度配额为脚本 2ms、后台结果与资源安装 1ms、回收 0.5ms、快照捕获 1ms；VM 至少每 256 个工作单位检查时间，分配与释放另计。单次不可分割操作同样测量，超限不能只靠总额度掩盖。GPU 上传按帧字节预算分批。快照暂停造成的业务排队时间单独记录，小场景目标一帧内完成。
资源预算起点为 VM 堆 32 MiB、CPU 解码驻留 64 MiB、GPU 纹理 128 MiB、桌面离屏工作目标 128 MiB；峰值包括待释放对象、候选模块和快照副本。超限先逐出可选缓存、延后预取，再报告具体资源需求。队列与回收积压纳入背压，不能无限增长。
每阶段必验：静止场景无无效脚本、布局和呈现；单滑条不更新无关布局；纯音量变化只通知音频；单节点淡化复用排版。开发运行和 release 都测体验，优化由同一工作负载的前后数据证明。

## 十四 开发顺序与验收
D01 固定公共协议、版本规则与验收样本，验证独立窗口与渲染接口、表面生命周期及能力选择；再验证 UI 分组绘制、变换命中和控件外观接入。队列按本文规则实现，D01 固定各目标默认容量并覆盖满载情况。
L01 实现语言最小闭环及宿主调用。S01 并行完成舞台事务、图片按钮、滑条、tab 和模态输入。R01 完成组透明度、裁剪、遮罩与缓存失效。M01 并行完成 Kira、资源取消和预算实测。以上共享 D01。
E01 验证剧情等待、作者组件绑定、历史与快进的会话契约。P01 完成对象图快照、跨进程读档、多槽和有限回滚。H01 在既有状态表示上加入热重载。完整语言与标准库按独立能力清单持续推进，属于最终验收，不能把语言最小闭环称为语言完成。
验收采用最小宿主夹具和针对性 UI 片段，不要求制作完整可玩流程。NullPlatform、记录型渲染器、假时钟和内存 IO 验证无隐式设备、时间或文件系统依赖；关闭 winit/wgpu 后检查核心及传递依赖。用例覆盖单次输入消费、配对按下与释放、拖动失焦、模态隔离、嵌套合成、保存后切场景、读档后旧结果、重载已有回调及节点销毁后的异步完成。音频、恢复与帧预算分别提供可重复的负载和观察值。协作执行器测单步最大耗时、满队列、取消与过期完成；表面丢失及挂起恢复测时钟和资源身份。Vita 承诺前先做 32 位交叉构建，再依次验证启动、输入、图片/字形、音频、可靠保存与效果，不承诺未经实测的帧率。
每个代理任务写明目标、允许目录、冻结的接口版本、已有代码、禁止扩大的范围、行为场景与性能预算。Issue 按可验收成果组织，PR 保持独立可审查；提交包含实际改动、相关用例、运行命令、观察结果与未解决项。不得以测试数量、模拟成功或完成报告代替实际契约验证。接口或依赖变更先提交设计修改，经负责人确认后继续。负责人亲自审输入、所有权、保存、音频和平台关键路径，重跑关键验证并处理跨模块冲突。性能计数器从第一阶段建立，协议变更同步三份设计。各后端的构建、设备和尾延迟验证属于对应实现任务。代码与文档以同一能力清单记录结果。

## 十五 UI 模块与呈现桥接合同
本节规定 reel-ui 的模块与行为边界。UI 结构整合不改变已验收的控件、CPU 帧记录、呈现几何和资源契约；后端与设备仍按各自用例验收。工作区采用 Rust 1.96.0 工具链。

呈现几何与上下文
reel-ui::app/core 导出几何、选择输入源并分发事件；bridge 管理 FrameId、surface epoch、呈现接受门和 NodeId↔WidgetId 的代际映射，沿用 reel-core 的共享预算。输入源明确为 Live 或 Presented，Presented 内可尚无接受视图。Reel 使用 Presented；首帧前几何命中为空，宿主关闭/挂起等平台命令照常处理。
几何快照包含全部登记节点，包括 stashed 节点。采用连续记录与 children 索引数组，每节点保存 WidgetId、parent 索引、children 范围、对齐 border-box 尺寸、border/paint insets、局部变换、border-box 到 window 变换、window 子树 bounds、已解析 content baselines、border-box 坐标的 clip 和 frozen 命中标志（content-box getter 只转换一次）。整帧保存 DPI、根及 id 索引。布局缓存、属性、action 和完整 WidgetState 留在当前运行时。路径沿 parent links 在有上限的复用缓冲中生成一次，整次事件借用；内存与构建成本计入候选帧预算。
每次真实指针分发固定一个 view，命中、捕获、祖先路由、hover、cursor 和所有输入上下文共用它。Context 借用 view 与已验证记录索引，QueryCtx 保持可复制；每个 context 无需克隆 Arc。自身几何 getter 的前置条件由构造入口保证。查询缺失或已销毁子节点时，QueryCtx::get、get_raw/get_raw_mut 等入口返回 Option/Result；Presented 缺失记录保持缺失，既不回退 Live，也不按当前控件几何重新命中。WidgetRef::children 依据所选拓扑枚举并过滤已销毁节点。布局、动画、普通 mutation 以及非指针状态更新显式使用 Live。
实现必须逐一迁移 event.rs、update.rs、contexts.rs、widget_ref.rs、render_root.rs、anim.rs 的 context 构造入口及其实际调用方；旧实验六文件范围只是原型范围。当前存活、stashed、disabled 状态继续即时生效；disabled 命中保留祖先消费路径，自身行为跳过。捕获失效与普通命中的 disabled 处理分别判断。
接受新视图仅发送实际路径差异的通知。需要退出的 hover/active 状态及失效捕获使用旧视图完成清理，再用新视图处理进入和静止 cursor；仍有效且在新视图中的捕获继续。清空 Presented 或切换表面代数时保留旧视图直到必要退出结束。Up/Cancel 在捕获目标回调开始时记录终止已交付，回调期间仍可观察捕获状态，删除与祖先冒泡只清理一次。

统一记录流
默认记录路径直接访问 VisualLayerPlan 的完整有序片段（§16）。Reel 导出在同一流中保留 Draw、Push/PopGroup、Push/PopClip 和诊断上下文；Scene 局部 ID 与源身份一起映射。效果组在 End 时整体合成一次，跨记录片段保持组与 clip 作用域。Imaging 图片、字体和 blob 转成带版本与预算的 Reel 资源引用，几何与绘制记录属于同一候选帧。replay-to-Scene 仅保留为显式调用的结构/CPU 参考。
独立片段是画序分段，消费时按原顺序组成完整流。使用稳定层边界时，paint_layer_mode 为 retained 声明：初始 Inline，缓存回调未执行时保留，显式设置 Inline 才恢复普通模式；其他 pre/paint/post 回调执行时，保留未重声明的层模式。
外部视频、材质或三维内容通过同一流的 ExternalDraw 接入。最小记录含宿主内容/资源 token、拥有 WidgetId、local bounds、content 到 layer 变换和确定画序位置；继承当前位置打开的 Group/Clip。其位置替换自身 paint 内容，保留 pre → own clip → external paint → children → pop clip → post。R01 实现已替换原先缺少作用域的 post-children placeholder；Reel 始终只有这一条画序来源，具体外部设备实现另行验收。

作者滑条
复用 Slider 的指针、键盘与 AccessKit 行为。slider.rs 提供只读规范化值/范围/步长、静默投影更新及变化标记，并让原 Widget 回调与作者包装器共用同一行为入口。Reel 包装器拥有轨道/滑块节点，替换全部默认绘制阶段，明确登记自身属性与默认值；装饰节点由控件统一处理命中和焦点。
SliderInputProjection 保存所选帧的 a/b、axis/direction、min/max/step、步进锚点及配置版本。中心与指针逆映射共用同一几何 helper；范围与步长配置通过统一规范化，范围改变而值未变也更新外观和可访问性。退化几何产生空的指针映射，避免除零；无效配置在提交前报告。
业务 value 由 Reel 当前模型管理，外部更新即时生效，旧帧显示值只用于绘制。指针按冻结投影产生绝对值，键盘/AccessKit 增减从当前业务值开始。配置版本仅随 range/step、axis/direction、端点策略等显式交互配置改变；普通布局尺寸、父级变换、动画与 value 更新保持版本。配置已变而对应帧尚未接受时，控件保留祖先消费，暂不执行旧投影行为；已有捕获沿失效路径交付一次 Cancel。新配置帧接受后恢复。
源码按职责组织为 reel-ui 内部模块，控件基础、树、呈现桥接不再拆成独立包。保留原始许可和必要版权/来源说明，普通外部依赖继续锁定引用；验证以当前行为、单一类型来源和依赖边界为依据，不做上游目录或补丁重放门槛。生产验收覆盖首帧/清空、缺失子查询、路径变更、单次终止、绘制组跨片段、外部内容作用域，以及图片滑条与业务绑定的同一数值/几何。

## 十六 可移植绘制帧与默认光栅后端

### 帧与导出

reel-ui::frame 定义 RenderFrame，保存帧身份、SurfaceEpoch、逻辑/输出尺寸、确定画序、分组、裁剪、外部内容、诊断上下文和资源引用。绘制数据复用 kurbo/Peniko 的几何与画刷描述；字体记录直接使用 Parley 已计算的 glyph ID、位置、字号、字体集合索引及 variation 坐标。渲染器接收这些结果，排版仍由原有文本系统负责。

导出直接访问 VisualLayerPlan 的有序片段与 imaging Scene 的公开 command/clip/group/mask/draw/context 数据。Scene 内的 ID 只在所属源 Scene 中有意义，映射键包含源 Scene 身份及局部 ID；若变换烘焙进数据，还需区分重放实例。mask 定义优先保持局部坐标，使用处携带变换，通过有界映射转成帧内索引。多个片段共用连续的 group/clip/context 状态，mask 子流使用独立验证栈。imaging 对 clip/group 深度分别验证；可移植记录同时保留独立逻辑栈及原画序。跨片段的正常嵌套完整支持；交叉结束的 clip/group 寿命缺少已定义的像素语义，导出以带位置的 UnsupportedRecord 明确拒绝该模式，避免按另一种栈规则猜测结果。IsolatedScene 表示记录边界，其本身不额外产生透明组。默认生产路径省去先展平为第二份 Scene 的复制，replay-to-Scene 保留作结构参考。

RecordedSource 移入同次真实 VisualLayerPlan、几何和候选身份。FrameBuild 持有其冻结租约，跨暂停复用这一份源记录；完成或取消后释放源租约。step 按命令、字节和工作额度推进，保存片段、命令、嵌套 mask 以及路径段、glyph、渐变 stop、字符串字节等 payload 子游标；全部遍历使用显式有界栈。每次扩容、路径/glyph/渐变复制与映射建立均先申请额度。单片工作结束返回未完成状态，硬容量或内容错误返回定位失败；结果完整后公开不可变 RenderFrame。生产导出使用可提前结束的 visitor，避免 infallible PaintSink 在失败后仍走完整条记录。

构建中和等待提交的候选共用容量限制。取消、失败或表面代数失效释放对应候选资格；几何和数据的实际驻留仍由最后租约计费。构建、上传完成都保持旧接受几何；协调器仅把精确候选的 PresentAccepted 送入既有接受门。Accepted 通知独立持有候选资格和同次几何，直到 resolve 或最后一个通知副本释放；允许 render_present → 创建通知 → 释放调用方 frame → 图形完成 → resolve 的同步顺序，不为等待接受而保留整帧资源。普通 key/几何引用不延长资格，未提交的 source/build/frame 在无接受通知时释放仍取消；通知不能复活已取消的候选。epoch、身份和接受顺序检查继续执行。图形完成通知独立决定后端资源回收。

### 资源身份与所有权

图片、字体和外部内容先在有界资源登记表中建立 ResourceVersion 与拥有型租约。复用 reel-core 的 AssetKey、ContentVersion，紧凑运行时句柄包含登记表代数及完整槽身份。源 Blob ID 只作为查找线索，登记时同时核对内容对象/版本，避免把碰撞身份当成相同内容。内容版本在资源准备阶段固定，导出只查表；缺少登记给出具体资源需求，由准备工作完成后重新构建候选。

登记表持有共享不可变数据，帧保存资源引用/租约，上传和在途提交各自取得其所需拥有权。同一图片或字体在多次绘制中复用数据。缓冲、索引表、候选副本、等待上传、图形在途与退休空间均计费；物理释放完成后退账。retire 停止取得新租约并移除登记表自己的保留；已有租约仍可用于提交，包括暂停构建、完整帧和已接受上传。全部租约释放且最后一次图形使用完成之后才归还实际存储，槽复用更新完整代数。导出的硬字节/工作额度约束新增帧、映射与登记存储。源 Scene 私有表容量、未被 command 引用的 mask 与共享 Blob 的底层分配属于生产者记录/缓存，公共 command visitor 只提供内容访问。R01 对这些源驻留分别测量 record、暂停构建、完成和取消时的实际分配/进程峰值及源清理耗时，明确标识源租约保留期；不以命令长度冒充精确源分配账本。后续需要源计划硬上限或分片析构时，增加真正的 owned-parts/drain/storage 接口后验证。

等待上传字节额度累计约束全部未完成批次的 payload 容量票据，与上传加驻留总额度独立检查；拒绝返回原批次。选定无强租约的资源槽后，先释放登记表自己的过期弱描述符票据再预留替代项，外部弱别名继续保留相应责任。

reel-render 定义后端及上传/呈现/完成/退休接口，读取 stage/core。RenderFrame 和资源描述保持平台无关；wgpu、窗口及平台句柄属于实现适配层。记录型/null 后端验证合同，提供明确的模拟提交结果；设备性能由实际后端测量。

### 外部内容与坐标

UI 外部层保留宿主不透明 token、拥有 WidgetId、内容局部 bounds 与 content-to-layer 变换。真实顺序固定为 pre-paint → own clip → external paint → children → pop clip → post-paint。外部 paint 前结束当前记录片段，继承已打开的组和裁剪；其后子节点继续同一顺序流。跳过缓存回调时保留上次声明的外部模式/token，显式切回 Inline 才恢复普通内容。

基底层与其他层使用一致的 layer-local-to-window 约定，绘制与 PresentedGeometryView 使用同次变换。内容坐标包含 border-box 的相应平移，clip 使用明确的 border-box 坐标。有限但奇异的变换可以保留视觉几何，输入逆映射返回空；导出转换使用显式上下文和前向变换，避免强制求逆。前向累计使用 UI 的完整 compose_local_transform，包含 layout origin 和 scroll translation；几何记录中单独的 transform 字段仅是其组成部分。非有限变换在变更提交/导出时给出定位错误。消除已知基底层变换偏差并通过像素/命中对应检查后，替换 U02 的临时 UnsupportedRootTransform 边界。

paint/append 边界另存连续命令的实际绘制者范围，普通 Inline 子控件的命令和定位错误不能以片段边界 owner 代替；范围游标在已计费的命令提交/片段推进时只访问相邻范围，初始化以固定一次索引读取得首个 owner；下一命令的映射预付款发生 Work 失败时也报告该命令的实际 owner，诊断不做未计费扫描或延后查找，payload 暂停继续保留相同来源。生成的 clip、调试框和检查器绘制也保存实际 owner。调试 bounding_box 已在窗口坐标中，使用 identity 片段；baseline 前向累计到窗口坐标，跨片段保持打开的作用域，不对奇异变换求逆。

组明确保存效果坐标空间、效果参数和保守 ink bounds。imaging Group 本身缺少整体效果变换，因此 paint/append 边界为组保存保证存在的 owner 与效果空间元数据；组的来源不能靠某个子绘制或可选诊断上下文猜测。bounds 包括笔画、字形外延、抗锯齿与滤镜扩张，布局盒仅用于布局。mask 自己的记录流、变换、alpha/亮度模式和采样规则随组保留。

### 默认后端方向

默认使用 wgpu 的常规几何与合成管线：图片四边形、缓存路径网格和字形图集四边形相邻兼容时合批。路径细分复用 lyon_tessellation，字形栅格化复用 Swash scale；保留现有 Parley 整形结果。候选依赖版本分别为 1.0.22、0.2.10，实施时锁定并检查实际依赖统一、许可证与目标构建。Swash 的 shaping 路径不进入此适配器。

颜色在合成前转为线性并预乘 alpha，输出到表面时按 SurfaceInfo 编码一次。图片准备按源编码/alpha 描述转换；纹理过滤采用线性预乘数据。渐变保留声明的插值规则，产生的采样颜色进入同一线性合成约定。灰度字形覆盖率与颜色分开，彩色字体图像按自己的编码转换。

普通绘制直接进入当前目标；需要独立组结果、mask 或 transition 时才使用离屏目标。EndGroup 对完整组结果应用一次 mask 与组透明度。alpha mask 使用覆盖率；亮度 mask 的覆盖率为 clamp(dot(linear_premultiplied_rgb, [0.2126, 0.7152, 0.0722]), 0, 1)，alpha 已包含在预乘值中。crossfade 对两份冻结输入的线性预乘结果按进度混合，遮罩转场沿用相同覆盖率与组机制。矩形裁剪可优化为 scissor，变换/path clip 使用真正的几何覆盖或 stencil，并保持跨片段、外部内容和离屏组的继承关系。

设备建立时确定并公开精度、采样数与效果能力。桌面质量档以 RGBA16Float 中间目标和 4x MSAA 为首选；受限档显式使用线性 RGBA8 与 1x 采样，字形覆盖继续保留。具体选择由设备能力与已配置内存档共同决定，创建后保持稳定；变更质量档触发缓存/目标版本更新。桌面离屏起始池为 128 MiB，与普通资源驻留分别限制并汇总峰值。1080p RGBA16Float 的 4x 附件加 resolve 已占 82,944,000 字节，深度、mask 与在途重叠另计；64 MiB 等更小配置在设备/尺寸协商时选择明确的受限档，或报告所请求质量的最小资源量。目标池按实际格式、sample count、resolve、深度/覆盖和同时在途数量预先计量，空间不足时保持既有画面并报告资源需求。质量和内存默认值最终以同一基础场景的实际目标设备数据确认。

Vello 0.8 的编码/渐变与 blend 路径采用 sRGB，imaging_vello 0.0.1 对 alpha mask/filter 另有缺失；因此它不是本合同的默认生产光栅器。CPU replay 可作结构与几何参考，线性颜色期望另用明确公式验证。

设备适配实施合同见 [R02-INTERFACE](../crates/reel-render/R02-INTERFACE.md)；该文档区分 device generation 与 SurfaceEpoch、实际队列完成和表面接受，以及独立目标/资源/临时预算。实现与设备验证状态以 R02 报告为准。

### 实施验收

R01 固定帧、资源租约、可恢复导出和外部内容顺序，保留 U02 的真实接受/输入回归。后续 wgpu 实施在同一合同上验证实际像素、alpha/亮度 mask、嵌套透明组、crossfade、字形与图片缓存、resize/挂起/设备丢失以及上传/回收边界。每份结果区分源码审查、CPU/null 和具体图形设备；完整帧、导出、光栅、合成、提交与物理完成分开计时。

R01 实施接口与边界见 [R01-INTERFACE](../crates/reel-ui/R01-INTERFACE.md)，真实控件、U02 回归与导出/驻留/清理测量见 [R01-REPORT](../crates/reel-ui/R01-REPORT.md)。自有图片/字体以 core OwnedBytes 将全 capacity 票据绑定到 Blob 底层，生产者缓存别名继续保留该账目；公开 API 导入的不透明源分配仍归生产者单独计量。输出、映射与登记表有硬额度，源 Scene 私有容量不由 command 长度推算。逐元素复制、映射和容量信用可跨 step 推进；实际容量分配与源 Drop 仍是原子操作，分别报告耗时。已知几何外延包含按冻结 DPI 计算的 AA；有限值运算溢出、本地 mask 的采样外延、Gaussian cutoff、字形轮廓与滤镜未知外延明确记为 Unbounded 并要求后端按真实输出目标裁限。记录型 PresentAccepted 是模拟接口结果，null 明确 Skipped，均不代表设备像素或 GPU 性能通过。

主要依据：
- imaging 0.0.1 公开记录：https://github.com/forest-rs/imaging/blob/eea0e47503fdab1c4159a939797ba74acab19876/imaging/src/record.rs
- 对应 Vello 适配：https://github.com/forest-rs/imaging/blob/eea0e47503fdab1c4159a939797ba74acab19876/imaging_vello/src/scene_sink.rs
- Vello blend 约定：https://github.com/linebender/vello/blob/302fa902ebc5a7605b6664393d43abaf369eee29/vello_shaders/shader/shared/blend.wgsl
- Lyon 细分接口：https://docs.rs/lyon_tessellation/latest/lyon_tessellation/
- Swash 栅格接口：https://docs.rs/swash/latest/swash/scale/index.html

## 参考来源
[1] Ren’Py 保存与回滚 https://www.renpy.org/doc/html/save_load_rollback.html
[2] Ren’Py Screens https://www.renpy.org/doc/html/screens.html
[3] Ren’Py Screen 优化 https://www.renpy.org/doc/html/screen_optimization.html
[4] 现有项目 https://github.com/jhq223/ons-rs 和 https://github.com/jhq223/krkr-rs
[5] Masonry 绘制与分层 https://github.com/linebender/xilem/blob/main/masonry_core/src/passes/paint.rs
[6] Quirrel 项目 https://github.com/GaijinEntertainment/quirrel
[7] Cargo features https://doc.rust-lang.org/cargo/reference/features.html
[8] Quirrel 模块 https://quirrel.io/doc/language/modules.html
[9] Kira https://docs.rs/kira/latest/kira/

[10] Rust Vita 平台支持 https://doc.rust-lang.org/rustc/platform-support/armv7-sony-vita-newlibeabihf.html
[11] VitaSDK GXM https://docs.vitasdk.org/group__SceGxmUser.html
[12] VitaSDK 内存 https://docs.vitasdk.org/group__SceSysmemUser.html
[13] VitaSDK 音频 https://docs.vitasdk.org/group__SceAudioUser.html
[14] Kira Backend https://docs.rs/kira/latest/kira/backend/trait.Backend.html
[15] Kira Frame https://docs.rs/kira/latest/kira/struct.Frame.html

