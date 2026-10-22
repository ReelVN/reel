# Reel 引擎架构设计
接口实现状态与规划分别见正文。

[文档目录](README.md) · [作者指南 中文](guide/zh-CN.md) · [Creator guide English](guide/en.md)

## 目标与文档边界

Reel 的目标是让创作者直观地表达想象中的画面和交互，包括自定义对白、UI、演出和嵌入的 2D 游戏。统一舞台承载画面、文字、控件与效果，函数组织组件，状态和活动表达交互与时间顺序；低端设备的内存、帧成本和输入体验同时进入设计。引擎复用 Masonry 的控件能力和 Kira 的音频能力，配套语言 Aside 以 Quirrel 为参照，用 Rust 实现编译器和 VM。

当前项目、接口和自有格式统一为 v1。快速迭代直接同步调用方、英文源码文档、规范和验证；必要校验集中在输入、资源和状态提交边界，内部遵循明确的不变量。
本文规定运行时所有权、输入与渲染、媒体、持久化和开发顺序。《Aside VM 与完整语言规范》负责语法、值模型、执行帧、回收和语言兼容；《Reel 游戏开发指南》负责作者接口、组件组合与示例。跨文档的等待、绑定和恢复语义必须一致。性能目标与当前行为合同分别标注，实测结果保存在固定版本的报告中。

## 一 产品范围与实现原则

作者通过共同舞台组合 UI、对白与二维演出；组件、模型和活动使用同一所有权与保存协议。

## 二 模块职责与依赖

通用库由独立的 `aside-std` crate 维护，通过 Aside host SDK 向任意宿主提供声明、模块接口和 native 调度。Reel 注册标准库并默认导入其 prelude；舞台、对白、音频和资源仍归 Reel。已有文本、集合操作继续共用 VM 实现，不复制到标准库。

`std::time` 提供 `now()`（UTC Unix 秒整数）、`parts(timestamp, timezone: null)`（year/month/day/hour/minute/second/weekday/offset_seconds 数据表，weekday 为周一 1 至周日 7）及 `format(timestamp, pattern: "%Y-%m-%d %H:%M", timezone: null)`。timezone 为 IANA 名称，null 使用系统时区；日期换算、夏令时和格式化复用固定版本 Jiff 0.2.38。无法识别系统时区时按 Jiff 的系统时区语义使用 UTC；显式无效时区、时间或格式抛带调用位置的错误。接口不启动计时器、不参与剧情时间或快进；作者按需要用已有活动更新界面。aside-std 保持独立，数据返回 VM 堆。

首批接口 `std::random(seed: null)` 创建独立随机对象；`int(min, max)` 取左闭右开整数，`float()` 取 [0,1)，`pick(items)` 从非空 array/tuple 返回原元素。复用锁定的 fastrand 2.5.0，同版本下固定种子与相同调用顺序复现同一流；省略种子由库创建新流。状态保存在普通 VM 宿主载荷中，绑定方法保留原对象；每次抽取只读写常数个值，不复制集合或创建堆载荷。非法范围和空集合在推进状态前报错。构造声明宿主效果，抽取声明写入效果，沿现有初始化与纯 derive 边界执行。

`std::math` 提供独立宿主的纯 float64 数学函数：sin/cos/tan/asin/acos/atan、atan2(y,x)、sqrt/hypot(x,y)、pow(base,exponent)、exp/ln/log2/log10、abs/floor/ceil/round，以及 radians/degrees。直接复用 Rust f64 运算，不持有状态、不分配 VM 对象，允许 derive 与初始化调用。三角角度为弧度，round 中点远离零；NaN/Infinity 沿 Aside float 语义返回，不伪造有效数值或隐式限幅；宿主节点继续在自己的属性边界要求有效范围。整数参数在标准库参数边界按 float64 转换，结果仍为 float；普通脚本的 float 参数与 host SDK 的 Rust f64 绑定保持严格浮点类型。
`std::math::curve(keys)` 是由 VM 堆拥有的不可变标量曲线；`.sample(time)` 沿普通 native 调用返回 float64。关键帧记录含 time/value、可选 mode（linear/step/cubic）与 in_tangent/out_tangent；切线以值/秒表示，cubic 为 Hermite 插值。构造接受有序非空 array/tuple，复制有限数值并预计算区间；重复时刻按输入次序保留跳变，采样取该时刻最后一帧，外部时间保持端值。构造捕获与验证分片计入既有堆/工作额度，外层数组变化报 ConcurrentModification；每个关键帧记录在单步内读取并复制。未完成对象仅由原生续体持有，取消后沿 GC 回收。只读对象、数值载荷、绑定 sample 方法和续体使用共同快照，不持有平行 Rust 数据图。

采样沿平衡树定位区间，再做固定标量计算，复杂度 O(log n)；每个关键帧使用现有八值内联载荷，读取无需外部索引。构造的临时关键帧数组沿 VM 分块索引寻址，建立树的总成本为 O(n log n)，结束后释放工作区引用。稳态采样不创建曲线存储或脚本轮询任务。构造允许初始化和纯派生，捕获模型字段沿同一依赖机制；在条件 derive 的隐藏分支省略 sample/time 读取即可撤销该来源订阅。曲线由引用决定生存期，调用者选择 clock、audio.position、滚动位置或任意有限标量作为采样时间。

Reel 的脚本服务通过独立 [Aside host SDK](aside-host.md) 接入。节点、会话和活动业务留在 Reel；可复用的 Rust native 声明、编译链接、运行时准入和属性 schema 生成归 Aside 扩展层。绑定宏从同一声明产生元数据与调用包装，不复制舞台类型或引入第二套语言实现。
工作区按职责划分，语言包可独立发布。
- reel-core：平台无关的资源/请求身份、预算/结果类型、AssetIo、Clock 与 WorkExecutor 契约；只含共享数据和基础服务接口。
- aside-vm：语言值、对象、调用帧、模块与执行接口。
- aside-compiler：解析、分析、字节码及源码定位，依赖 VM 公共格式。
- aside-lsp：编辑文档、语言查询和 LSP 传输，复用编译器；宿主通过注册接口提供项目环境。
- reel-ui：控件、布局、属性、交互与舞台桥接。core/app/layout/properties/widgets/theme 管理 UI 基础，bridge/frame/resources 管理呈现接受、可移植帧与资源租约；passes/tree_arena 为私有实现。
- reel-render：后端接口、资源上传、缓存与分组合成，读取 UI 提交结果。
- reel-media：Kira 适配和播放控制，视频作为可选模块。
- reel-engine：会话、剧情任务、宿主绑定、资源与持久化协调。
- reel-player：平台适配、窗口与事件循环，组装引擎、渲染和媒体输出。
- reel-cli：检查、开发运行和构建，调用编译器与播放器库入口。
依赖方向固定为 ui→core，render→ui/core，media→core，player→ui/render/core；engine→aside-vm/aside-compiler/aside-std/ui/core，player→engine，cli→compiler/player，compiler→aside-vm。Aside 保持独立于所有 reel 包。语言包不依赖窗口、舞台或媒体。业务数据类型只在所属模块定义一次，跨线程传递拥有自身数据的消息或线程安全只读缓冲，不传 VM 堆引用。渲染器通过资源 ID 与版本取得内容。

动态画面沿用 image 节点、ResourceLease 与普通 FrameImage 绘制。reel-core::image 定义 Yuv420Image、Yuv420Storage 和显式 YuvColor（BT.601/709 矩阵、完整或有限范围、BT.709 或 sRGB 传递函数）；8 位 Y/U/V 平面由生产者 Arc 与预算票据共同保活，UI 不依赖媒体解码器。每张帧资源不可变并拥有独立版本；舞台接受直接资源引用，跳过路径解码。录制的图像资源绑定进入既有绘制命令所有权元数据，保持裁剪、变换、透明度和层序。GPU 直接上传三个 R8 平面并在采样时转成线性 RGB；仅在旧资源与在途/已准备画面均已释放后复用纹理，不通过重写仍被旧画面引用的纹理发布新帧。注册表 release 仅放弃自身持有和新 acquire，允许已有租约进入尚未编译的画面；retire 仍用于显式失效。首帧未到使用 ImageSource::empty，不读取占位路径。帧截止时间、PTS 和播放终态由媒体协调器负责，不进入 UI 的像素协议。
维护中的库统一属于工作区 crate，不保留 third_party 或 crates.io patch 层。reel-text 负责文字整形、布局及 ICU 中日分词与复杂文字换行；reel-audio 负责混音、声音与退休数据的移交，设备输出由 reel-media 唯一负责；aside-syntax 负责不可变语法树、源码位置及共享节点所有权。三者分别源自 Parley、Kira、rowan，许可证及简洁来源说明随 crate 保存，后续按 Reel 的职责和接口直接维护，不承担上游目录、版本或补丁同步要求。
aside-syntax 在所有权层通过原子引用计数取得唯一释放权，并迭代释放深树，覆盖独立节点克隆和跨线程最后引用。其继承的底层指针实现封装在语法树 crate 内；aside-compiler、reel-ui、reel-text 和 reel-audio 的生产代码保持 unsafe_code=forbid。语言仍使用单一递归下降/Pratt parser，长左结合运算和后缀链按显式工作表处理。
通用库使用锁定的普通依赖，包括 winit、wgpu、image、serde、atomic-write-file、codespan-reporting 和 Logos；文本、集合与数学分别复用 Unicode 库与 glam，VM 对象槽使用 slotmap。视频适配器使用固定版本 OpenH264、mp4、h264-reader 与 Symphonia；资源归档保存原始条目并使用 BLAKE3 校验，具体格式见“字节码与资源分包发布”。借用的 UI 控件、布局与呈现几何只维护在 reel-ui；不引入重复的窗口、设备输出或 UI 类型实现。
Quirrel 的 C++ 实现和独立兼容运行器退出工作区，完整原测试输入、预期输出与原 runner 参照保留在 aside-compiler 的测试 fixtures 中。迁移用例由同一 Aside 编译器生成 Aside 字节码，编码/解码后交给真实 VM，在普通工作片和极小工作片交错 GC 下分别核对原断言与输出。解析通过、原 Quirrel 通过和 Aside 执行通过分别记录；未迁移用例不能删除或计为通过。来源固定于提交 19f17df9b6ee43c91ef5c635fd45970ec76c5dbe。[6]
UI 代码最初借用自 Masonry 基线 271a27a6d4a930f7878d404f9014e3c50a3a9b88。[5] 当前仅维护 crates/reel-ui 一个 UI crate，版本跟随 Reel workspace；原 masonry、masonry_core、tree_arena、reel-stage 的实现按职责合为内部模块，不保留兼容转发包。安全树为私有模块，文档截图直接引用，不再保留 include_doc_path 宏包。许可、版权及必要来源说明保留；不要求与上游目录、版本或源码哈希持续一致。reel-render/reel-player 分别承担后端与平台组装，UI 不反向依赖它们。
借用实现由对应 workspace crate 自主维护，保留原许可证、版权及简洁来源说明。来源记录不构成持续同步源码、目录或哈希的约束；测试迁移保留原输入及 oracle，不把原编译器专属输出当作 Aside 必须模仿的 API。
当前 reel-engine 直接依赖 aside-compiler；按 source feature 裁剪编译器属于后续构建目标。模块热替换协议属于规划；当前编译 feature 以各 crate 的 Cargo.toml 为准。编译器、监视器和对照运行时留在开发侧，精简构建检查实际依赖树。[7]

## 三 平台接口与移植边界

### 视频舞台与媒体活动

视频解码使用锁定版本的 OpenH264、mp4、Symphonia 与 h264-reader。

`video(source, ...image 节点属性)` 创建空的普通图片节点，`.play(looping:false, volume:1, start:0, end:null, owner:null)` 启动并返回可等待活动。一个节点同时一个播放，重播停止前一个；节点或显式 owner 退休取消播放，隐藏不暂停。pause/resume/stop 沿共同活动协议，video.seek/position 使用秒。有限视频支持 finish；循环视频须 stop。停止与自然结束保留最后显示帧，节点退休释放资源。存档只保存路径、播放区间、当前位置、暂停与归属，不序列化已解码帧或设备句柄；读档重开资源并重建首个适用帧。

MediaBackend 统一音频和视频的设备工厂、维护、取消与退休；视频音轨进入同一 Music 总线，有声视频以实际消费的 PCM 为时钟，静音视频使用排除宿主挂起的会话时间。码流分段读取、解码和队列均在后台，显示采用已有舞台图片的资源租约和三平面 YUV 纹理；队列固定容量，旧帧租约持有期间不可覆盖其纹理。不在每帧复制整幅 RGBA 或重新创建纹理。CPU 验证覆盖实际解码、音频时钟、暂停、跳转、结束、取消、舞台退休与快照恢复；设备显示由目标平台另行验收。

### HAL 与公共呈现

内置桌面使用公共 PlatformHost 与 Presentation，其他平台复用同一会话、输入、记录、已接受帧与生命周期。平台适配器使用 reel-core 的时钟合同和共享会话接口。

PlatformHost 的当前必需命令为 request_redraw、set_wake_deadline、before_present、request_exit，时钟沿用 Clock，后台通知沿用 WakeSignal。原生事件由适配器转换后进入共同 UI/会话；窗口创建及原生表面创建仍在组装代码。桌面剪贴板和 IME 已由可选平台适配器接入，系统可访问性适配仍待实现。

`window.info()` 返回当前窗口快照：width/height 为客户区逻辑像素，scale_factor 为物理像素倍率，windowed_width/windowed_height 为窗口模式尺寸，fullscreen/vsync 为当前状态，capabilities.resize/fullscreen/vsync 表示实际可修改项，resolutions 为当前显示器去重后的逻辑尺寸列表。只读 platform 为宿主声明的平台：windows、macos、linux、android、ios、web 或 unknown；Winit 按实际编译目标填写，无窗口或未声明的平台填写 unknown。platform 不代表设备能力，交互仍按 capabilities 判断。列表仅在显示器或 DPI 改变时刷新，不逐帧枚举。`window.configure(width:null,height:null,fullscreen:null,vsync:null,owner:null)` 返回可 await 的活动，完成值为应用后的同结构快照；省略项不修改，尺寸为正有限数，全屏明确为无边框，尺寸只控制窗口模式并在退出全屏时恢复。更改中的窗口拒绝另一项更改为 Busy，不积累请求；不支持的能力在原调用处明确报错。取消等待者不撤回已提交的平台更改，owner 退休取消结果，操作提交前取消可撤销请求。窗口状态不由剧情存档回滚，恢复后调用 info 读取当前宿主；脚本保存的旧快照仍是旧值。未完成请求使快照 Busy，完成值可保存。平台值属于已有 WindowInfo 投影，没有新增轮询或单独缓存。

窗口服务使用 reel-core 中的平台无关数据、engine 请求/结果和 player 的可选 WindowControl 适配；Winit 处理原生尺寸与全屏事件，RenderBackend 的可选 vsync/set_vsync 控制呈现同步，默认适配返回不支持。WGPU 默认 FIFO，关闭同步选择真实支持的 Immediate 或 Mailbox，恢复设备/表面保留同步选择；不因关闭同步添加空闲重绘循环。无窗口宿主 info 返回零尺寸及全 false 能力，configure 不伪成功。尺寸/全屏请求收到平台实际结果后才完成，画布 game.configure 与窗口客户区保持独立。

RenderBackend 保留配置、呈现、完成轮询和挂起责任，使用具体错误类型及统一的拒绝/设备丢失/表面失效分类。呈现通过宿主的 before_present 通知后才交给平台。RenderFrame 已拥有全部 CPU 资源租约；后端可以在同一次提交内上传，独立 upload 是可选预热能力，上传额度为零的后端明确拒绝而不影响普通帧资源。资源生产者通过 ResourceRegistry 退休资源；后端 retire 退休其持有的资源，既有已接受帧租约的重绘仍合法。无持久 GPU 缓存的后端按提交保留和释放帧租约。完成轮询不得丢失超过本次额度的结果，完成通知和呈现接受保持独立。

公共呈现协调器管理画面更改与曝光重绘、候选几何、已接受帧、失败后的重试和表面代数。任何失败/跳过都结算对应候选，不能积累无主候选或发布未呈现输入；永久拒绝后只重绘最后已接受帧，提交完成才解除临时压力。原生 GPU/窗口类型只出现在各自适配模块。无桌面宿主与默认桌面调用同一协调器。

会话逻辑时间排除宿主挂起区间；VM/GC 截止时间继续使用真实单调时间。恢复先更换表面代数与输入几何，再恢复逻辑时间；关闭即使在挂起期间仍分片清理任务。验证真实源码的输入、动画、挂起恢复、拒绝/旧帧和关闭，保留既有呈现、预算和终态合同；无桌面/启用适配器依赖树、debug/release、Clippy 分别检查。关注空闲无轮询、无额外逐帧像素副本、完成事件不重复分配队列，以及租约实际释放；CPU、软件 GPU 与原生窗口证据分别陈述。

窗口和渲染分别选择，默认组合为 WinitHost 与 WgpuBackend。reel-player 的 winit feature 单独提供 platform::winit::WinitHost，不依赖 WGPU；window feature 组装内置 Winit + WGPU 播放器。reel-render 的 wgpu feature 单独提供 gpu::WgpuBackend，不依赖 Winit。reel-player/platform 定义 PlatformHost，reel-render 定义 RenderBackend，reel-media 定义 AudioOutput；AssetIo、Clock、WorkExecutor 在 reel-core 中定义。player 组装适配器并转换输入，engine 与 reel-ui 不反向依赖 player。UI 控件与布局同窗口外壳和 GPU 后端分开，以依赖检查和限定构建验证隔离。
原生窗口或固定显示目标的创建、表面来源协商与销毁留在具体 player 组装代码，不进入公共 trait。可借用或包装原始句柄，但有效期必须覆盖表面使用期；先停止提交并释放表面，再销毁显示目标。winit 事件、wgpu 设备与纹理、Vita 原生句柄都不得进入会话、RenderFrame 或 DSL。每个后端负责上传、格式转换、离屏目标、设备重建和完成后回收，不能仅替换最终绘制函数。
Cargo features 决定编入哪些适配器，运行参数只在已编入且能力匹配的组合中选择。游戏包声明必需能力；缺失时报错，有声明的低质量后备才可降级。reel-ui 与默认外壳、Kira 与默认输出的目标依赖都要检查，关闭桌面 feature 后不能仍被传递依赖强制引入。不建设动态插件系统或通用平台框架。
以下 Rust 签名概括模块边界，实际定义位于 reel-player/platform、reel-render、reel-media 与 reel-core。自定义类型由所属模块维护，Send + Sync 只约束实际跨线程的数据。
```rust
pub trait PlatformHost {
    fn request_redraw(&mut self);
    fn set_wake_deadline(&mut self, when: Option<MonoTime>);
    fn before_present(&mut self);
    fn request_exit(&mut self);
}
pub trait Clock {
    fn monotonic_now(&self) -> MonoTime;
    fn wall_time(&self) -> Option<UnixMillis>;
}
pub trait RenderBackend {
    type Error: BackendError + 'static;
    fn capabilities(&self) -> RenderCaps;
    fn configure(&mut self, info: SurfaceInfo) -> Result<(), Self::Error>;
    fn upload(&mut self, batch: UploadBatch)
        -> Result<UploadId, Rejected<UploadBatch>>;
    fn render_present(&mut self, frame: &RenderFrame, before_present: impl FnOnce())
        -> Result<PresentOutcome, Self::Error>;
    fn retire(&mut self, resources: &[ResourceVersion]);
    fn poll(&mut self, out: &mut Vec<RenderEvent<Self::Error>>, limit: usize)
        -> Result<(), Self::Error>;
    fn has_pending(&self) -> bool;
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
以下字段规定接口的语义、单位、所有权与终态；结构体字段以所属模块定义为准。PlatformCaps 的完整平台服务能力集属于规划，当前可用能力由实际适配接口报告。
- 身份：各类请求 ID、FrameId、表面代数使用相互独立的 u64 包装类型。Scope 为 Session(session_id, epoch, owner_id) 或 Process；保存写盘使用 Process。MonoTime 是单调纳秒 u64，UnixMillis 是可缺失的 UTC 毫秒 i64，均不含本机指针。
- Rejected<T> 保存原始 request:T 与 reason。reason 至少区分 Busy、Unsupported、InvalidRequest、Unavailable、OutOfMemory 和服务故障，附拥有型诊断文字；只有 Busy/Unavailable 等明确可重试原因允许原请求重试。拒绝不产生已接受 ID 或业务副作用。已接受请求的终态为 Completed(payload)、Cancelled、Failed(error)，结果记录包含请求 ID 与 Scope。Cancelled 用于释放与记账，不重新唤醒已取消业务。
- QueueLimits 含 max_requests:u32、max_bytes:u64；BudgetTicket 含预算类别和 bytes:u64，随拥有的数据转移，最终释放一次。接受请求时同时保障完成结果的队列容量。工作预算分别预留输入、最大输出、临时工作区和完成记录；解码大小尚未知时先有界探测头部，再在大分配前取得输出额度。CPU 输入、解码结果与 GPU 资源各自持有票据，上传过程中的重叠驻留分别计量。WorkBudget 含 max_units:u32、max_bytes:u64、deadline:Option<MonoTime>；WorkProgress 报告实际消耗与是否仍有工作。
- PlatformCaps 含指针、触摸、手柄、文本输入、剪贴板、可访问性、挂起恢复、等待及唤醒支持位。TextInputState 含启用值和逻辑坐标的插入点矩形。输入事件带输入序号、设备身份和事件时间；尺寸事件同时携带物理像素、逻辑尺寸和缩放系数。
- SurfaceInfo 含 physical_size:[u32;2]、scale_factor、surface_epoch、color_encoding 与 alpha_mode；零尺寸代表暂不能呈现。RenderCaps 含支持格式/效果集合、纹理与离屏尺寸上限和上传队列限制。UploadBatch 拥有资源版本、尺寸/stride/格式、像素或网格数据及预算票据；所有乘法和长度校验先于接受。
- PresentOutcome 为 PresentAccepted(FrameId)、Skipped(reason)、SurfaceUnavailable(reason)。RenderEvent 至少有 UploadFinished(id,result)、ResourcesRetired(versions) 和 DeviceLost(error)；可选的实际呈现反馈另带 FrameId 与时间，不能替代资源回收通知。返回的 FrameId 必须对应本次提交帧。
- ReadRequest 含资源身份与可选固定版本、offset:u64、len:u32、优先级、Scope 和预算票据；读取结果拥有 bytes 并返回确定版本，明确 eof，长度不得超过请求。SaveWrite 含槽位、提交序号、完整槽位数据、最低持久提交级别与预算票据。IoCaps 声明范围读取、读取/保存队列限制及可提供的提交级别；只保证可见替换、不保证持久性的实现不得报告持久保存成功。
- WorkRequest 含 kind、拥有型输入、Scope、优先级及预算票据；kind 限于已注册的解码、编码、校验操作。WorkCaps 含 Threaded/Cooperative、支持种类和队列限制；不支持的 kind 在接受前拒绝。处理进度不算终态。
### 平台与时间
PlatformHost 提供命令，原生事件循环转换输入后驱动公共会话与 Presentation，不要求平台提供阻塞的 pump_events。PointerEvent、TextEvent 和 WindowEvent 沿用 reel-ui 的共同数据；原生生命周期驱动 Presentation、RenderBackend 和 StoryRuntime 的挂起/恢复。Presentation 只接受严格递增的表面代数。组装方在恢复时先失效旧输入，再配置后端，成功后恢复剧情时钟；VM 截止时间不扣除挂起区间。桌面适配器已接入剪贴板、IME、触摸和 Gilrs 手柄输入；系统可访问性适配与各目标的实体设备验收继续独立推进。后台通知复用 reel-core::WakeSignal，实时音频的通知路径仍须单独验证；不把通用平台回调直接作为实时保证。
完整平台能力集规划包含输入、文本服务、可访问性及等待/唤醒；重绘可合并。MonoTime 使用固定单位的 u64，不保存 std::time::Instant。会话逻辑时钟独立累计，可暂停与恢复；墙钟可缺失，只用于显示日期等。恢复后重设截止时间，系统睡眠不得变成巨大动画增量。
### 渲染与音频
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

### 资源与工作执行
作者图片与字体共用 AssetIo/WorkExecutor 装配。开发资源以 AssetSource（资源名与可选固定版本）请求；describe 返回长度与可用版本，read 返回本次不可变字节的确定 ContentVersion。没有预先版本索引的目录无需启动扫描；默认目录适配器在后台首次 describe 时分块计算内容版本，随后支持范围读取。发布/内存适配器可提前提供固定版本和范围读取。解码输入使用 Arc<OwnedBytes>，探测与解码共享同一份输入及预算，先后台探测尺寸，再取得输出/临时额度。普通后台队列复用同一终态和取消合同，不由图片节点创建线程；字体准备期间保持旧画面，完成后统一发布设置与舞台。

默认窗口的资源验收包含实际 DPI 和裁剪所需临时目标。WGPU 准备帧前按完整组、裁剪、遮罩与转场计算同时存活的目标峰值，再在既有质量档位中选择能容纳整帧的档位；不以单张纹理能容纳就假定整帧可行。只有裁剪、没有独立遮罩的组，将覆盖率、组透明度和源图直接合成到父目标，不再构造中转纹理；保持原来的预乘色与 source-over 结果。此路径纳入 reel-render 的像素验收，沿用帧接口和预算，不增加作者参数。

离屏目标按尺寸、质量和帧内分配槽复用，提交后才发布到同一 Renderer 的缓存。每次取用目标都清空，包括帧内回收后的再次使用；绘制和最终呈现/截图复制在同一命令缓冲内结束，同一 GPU 队列后续提交可以复用这些目标。Prepared、提交和缓存共享实际分配的预算所有权，不按引用重复计费；单帧需求仍独立计账。尺寸、质量或所需槽数变化时退休多余缓存，尚未完成的 GPU 引用保留预算。临时提交压力沿完成通知重试，不作为永久拒帧反复报错；不放大默认预算或增加主线程等待。

Aside 的批量容器操作由 VM 自身执行，不经 Reel 宿主。数组插入、删除、扩缩、反转和扩展共享可恢复的工作副本与一次提交路径；参数名绑定在修改前完成。提交只交换受管理的存储，保持对象身份和预算总量，并使增量 GC 重新扫描移动后的边；取消由现有帧根清理处理。不得为了批量操作引入一次性整数组复制、扫描或释放的主线程停顿。

Aside 文本基础方法复用 Rust 标准库的 UTF-8 解码与标量迭代；分片只保留至多一个未完整标量和位置，不因字符串总长申请临时整串副本。切片与连接使用受计量的私有字节缓冲，完成时原地封为 string。文本连接不反复拼接已生成前缀；来源集合版本、取消与错误沿用 VM 所有权路径。

ReadRequest 使用 AssetKey、版本、offset:u64、受限 len:u32 和优先级，不要求路径、mmap 或文件监视器。AssetIo 可读包、内存或平台文件系统，接口须快速返回；真正可能阻塞的文件调用交给平台异步机制或 IO 工作者。SaveWrite 使用应用保存命名空间和独立进程请求身份；完成只表示该平台已验证的持久提交级别。缺少可靠原子替换的平台须采用经故障验证的双槽或版本日志，不能假定 rename/fsync 一定适用。
WorkRequest 是图像解码、快照编码、包验证等有限工作描述，拥有输入、优先级、取消身份与预算票据，不接收脚本闭包。WorkCaps 声明线程或协作模式及支持种类。协作实现保留游标，在工作单位、字节或截止时间到达时让出；不能用一次阻塞整图解码冒充分片执行。无工作线程且无可分片实现时，明确不支持该工作或要求预转换资源。实时音频供给不能被普通协作任务饿死。
所有队列同时限制数量和字节。拒绝返回原请求及原因，不丢数据、不暗中改为同步执行。完成消息拥有自身数据，包含请求身份、所有者或会话代数，最多一个终态。业务取消先撤销对应等待/订阅，再请求底层取消；结果路由始终核对等待仍有效及 Scope 匹配。服务尚未结算时取消产生唯一 Cancelled；已结算且排队的终态保持原结果，交付后只用于额度与进程账本结算，已取消业务保持终止。这样已发生的保存提交仍有准确记录，旧读取或工作结果也无法重新唤醒已取消任务。poll/drain 每次最多写入 limit 个事件，调用方预分配容量。IO 读取与保存各自保留容量；完成消息通过两路轮转等有限等待策略交付，持续读取也须让已完成的保存及时出队。
### 桌面与 PS Vita 目标
首批桌面目标是 Windows、Linux、macOS，各组合完成构建和行为验证后才列为支持。移动端与 Web 需要另外适配生命周期、输入、媒体与存储。PS Vita 是未来移植目标，当前接口不代表已经可运行。
Rust 的 armv7-sony-vita-newlibeabihf 目标为 Tier 3，依赖 VitaSDK，标准库部分支持且无预编译标准库；VitaSDK 是开源 homebrew 工具链。[10] 因此先验证锁定工具链和完整依赖树，不从 target 存在推导 reel-ui、Parley、Kira 等已兼容。
Vita 的 GXM 有专门纹理布局、内存映射与同步要求，GPU 预算需按不同内存区域实测。[11][12] 音频提交可能阻塞，应由专用路径处理，输出适配器按实际端口把 f32 转换为所需 PCM，转换缓冲预分配。[13] 32 位构建不保存指针或 usize 布局；长度转换、stride×height 与对象索引做溢出检查，不假定 AtomicU64 可用。桌面 N100 的帧时间和资源预算不直接用于 Vita。

## 四 线程与帧调度
主线程拥有窗口、舞台、UI 和 GPU 提交。VM 在主线程按预算执行，避免细粒度跨线程调用。默认桌面把读取、解码和存档编码交给工作线程，其他平台依所选 IO 与执行器契约推进；音频由独立输出路径保障。后台队列同时限制任务数和数据字节数，当前可见资源优先于预取。
每帧固定执行：输入分发；预算内脚本与后台完成消息；动画采样；状态提交；必要布局与排版；命中几何与绘制计划更新；呈现。布局属性动画参加本帧布局，纯变换与透明度只影响合成和命中。静止画面等待输入、计时器或资源事件，活动动画才持续请求新帧。
输入采用最后一次被呈现系统接受的帧几何，收到 PresentAccepted 才替换快照。单次输入固定使用同一份快照；获取表面、提交或呈现失败、过期，以及 Skipped 或表面不可用时都保留原快照。该规则不承诺屏幕已经扫描显示。命中后核验当前节点代际与交互有效性，已销毁节点不再接收事件。一次输入最多推进一次剧情。
reel-ui 的交互分发层是唯一控件输入入口。它使用冻结的 PresentedGeometryView 进行定向分发，该视图包含 NodeId/WidgetId 映射、局部尺寸、逆变换、裁剪、层序和祖先路径。按下、释放、hover、捕获坐标及 EventCtx 的几何查询都读取本次输入选定的视图，保持同一依据。定向事件入口按冻结几何分发，禁止随后又按 widget_arena 的当前布局二次命中。已销毁或禁用节点由当前有效性检查过滤。控件先处理并消费输入，剩余推进事件才进入剧情。按钮配对按下和释放，滑条捕获拖动指针。模态容器隔离下方指针、焦点、键盘和剧情快捷键，并释放作用域外的捕获与快进保持；失焦清除捕获与按键保持。

## 五 统一舞台与合成

### 批量舞台粒子

`particles(source, seed:0, ...节点属性)` 创建静止的单个舞台节点，共享精灵、种子与坐标空间。source 与 seed 只读；共同变换、可见性、透明度与裁剪仍可更新，作用于该节点的所有批次。图片沿共同资源加载与源码诊断路径。source 必须是完整 PNG/JPEG 精灵；分块 `.reel-image` 只供 image 节点使用，粒子请求在资源边界带构造源码位置明确拒绝。

`emit(node, burst:1, rate:0, duration:null, lifetime:5, lifetime_spread:0, velocity_x:0, velocity_y:80, spread_x:20, spread_y:20, acceleration_x:0, acceleration_y:0, size:12, size_spread:4, size_end:null, opacity_start:1, opacity_end:0, spin:0, spin_spread:30, x:0, y:0, owner:null)` 追加一个批次并返回独立活动；不会停止或清除此前批次。每次 emit 捕获节点 width/height 为发射矩形尺寸（缺省为零），x/y 是本批在节点本地坐标中的矩形原点，接受正负有限数，调用时固定。修改节点 x/y 仍整体移动所有批次；不同本地出生点无需拆成多个节点。duration 为非负有限发射秒数，null 连续发射。burst 粒在启动时刻同时出生，随后每 1/rate 秒出生一粒，有限发射在 duration 截止（含该时刻），最后一粒实际寿命结束后返回 Completed。rate:0 只产生瞬发粒子，是可 finish 的有限活动；rate 与 burst 均为零时立即完成。duration:0 仍产生 burst，有限时段内没有出生粒子时立即完成。pause/resume 只冻结/继续本批的发射与运动；stop 只清除本批并返回 Stopped；finish 只接受有限活动，清除本批并返回 Completed。目标退休取消全部批次，owner 退休取消属于它的批次；等待者取消不取消生产者。owner 缺省沿用作用域/任务/回调节点，主剧情归会话。终态独立于节点保存，可重复 await，结束后释放目标/owner 根。隐藏不暂停。

速度单位为逻辑像素/秒，加速度为像素/秒²，spin 为度/秒；spread 是围绕基值的均匀半幅，size_spread 不超过 size，lifetime_spread 不超过 lifetime。lifetime 为正有限秒数；rate、size、显式 size_end 与各 spread 为非负有限数，burst 为非负整数，透明度在 [0,1]。每粒寿命在 lifetime±lifetime_spread 内确定性取样，零寿命立即结束；size_end:null 保留每粒随机出生宽度，显式终值使宽度在该粒寿命内线性变化，保留图片宽高比。每次调用在入口完整验证本批参数；失败保留已有批次及节点的下一发射序号。参数已从 particles 移到 emit，不保留旧构造参数；默认从 rate:60/burst:0 改为 rate:0/burst:1，即默认发射一粒。

每批配置和累计逻辑时间形成不可变投影，按出生序号解析位置、匀加速运动、旋转及寿命内线性尺寸与透明度。节点 seed、成功发射序号、批内出生序号与取样通道共同确定 SplitMix64 取样；同种子、调用顺序和时间得到同一效果，与分帧无关，追加的一滴不会反复使用第一滴的随机样本。出生 x/y、速度 x/y、尺寸、角速度使用六个通道，寿命使用第七通道。瞬发序号从 1 到 burst，后续定时出生沿其后递增。每批仅保存解析配置与时钟，不创建逐粒节点、任务、回调或逐粒状态数组。绘制按追加顺序枚举各批最大寿命窗口内的候选，再按实际寿命过滤；大时间跳跃直接跳过必然已死序号。有限活动逆序计算实际最大死亡时间，较早出生的最大可能死亡时间不超过已知终点时停止；这项一次性工作按候选纳入维护切片预算，固定寿命终点为常数工作。

粒子维护按已采样时钟、节点退休版本及显式作用域退休推进；普通文字、布局或透明度属性发布不触发所有权重扫。粒子节点与其他节点可以同时 tween，完成本轮时钟与属性投影后即可接受中间帧并结算 stage.presented，无须等动画终点。目标或 owner 退休仍取消其批次，包括暂停批次；停止和恢复沿同一活动与退休检查路径。维护缓存以恢复后实际退休版本初始化。

活动根、活批队列及完成扫描状态按实际容量计入 VM HeapReservation；不可变批次投影按实际容量及共享所有权开销计入独立的 CPU 输出驻留 BudgetPool，字节额度沿 VM 字节额度派生，不新增作者参数。这是两个独立额度，不把投影驻留计为 VM 总堆，也不把两者当作进程 RSS。投影更换时新旧并存均计费；旧帧及跨线程消费者持有 Arc 与 BudgetTicket，SessionVm 结束不提前退账，最后引用释放后才退款。时钟推进只更新节点时间，不复制批次配置；追加、控制或死亡时才更新批次集合，每次复制与该节点当前活批数成正比，恢复逐批重建同一集合。准入失败保留旧集合，有限活动终点维护沿既有分片预算。绘制仍按实际可见候选工作，不以粒子数固定截断，也不声称同步绘制录制为硬实时。

使用现有会话活动时钟，资源等待/宿主暂停沿播放器现有冻结规则。快照逐批保存配置、发射矩形原点与尺寸、累计时间、暂停、归属及终点计算；捕获前先通过既有舞台维护屏障完成待办维护，节点下一发射序号也保存，恢复不重发已有批次。GPU 在既有绘制流中把相邻同图片矩形合并为有序 quad 批次，共用顶点/索引上传与纹理绑定；clip/group、非图片绘制和资源变化结束批次，逐图片变换、裁剪来源及 alpha 保留，不新增特效渲染器或帧 ABI。

### 保留画面的舞台转场

`stage.transition(change:, target:null, duration:0.3, easing:"linear", mask:null, mask_softness:0.06, mask_invert:false, mask_x:0, mask_y:0, owner:null)` 在当前任务执行完整转场并返回零参数 change 回调的原返回值，调用本身等待完成，不再在外面 await。并发沿用 spawn；初始化、纯派生及 NoWait 边界拒绝调用。target 为仍挂载的舞台正文节点时仅过渡该节点及其子树，null 为整个逻辑舞台正文，窗口光标保持独立。目标节点应保持身份，通过 change 修改其属性或子节点；image 可直接更新 source，text 可直接更新 value；整个舞台的 change 可 stage.replace。duration 有限且非负，easing 使用 tween 的内置具名曲线，mask 使用已有字符串或 MaskExpression，参数沿已有节点遮罩规则；null 用统一进度混合。duration 为零直接执行变更并等待新画面接受，省去旧图保留和计时。

阶段为 OldPending → Changing → NewPending → Playing → FinalPending。先等待稳定舞台实际接受，按这份画面的真正子树边界保留旧绘制与资源，再执行 change；回调可等待，其普通活动与新侧内容沿原所有权继续运行。新资源、布局和零进度合成帧接受后才开始时间；最终进度为一的帧接受后返回并释放旧画面。相同或相互包含的目标在已有转场期间请求另一转场会在执行新 change 前报告目标冲突；独立子树可并行。change 中的交互等待应放到转场返回后：转场目标的输入、焦点和确认准入在整个过程暂停，外部 UI 照常；此门控属于宿主呈现状态，保留作者 enabled/visible 值及绑定。取消后按当前真实舞台的下一接受帧恢复准入。

旧侧是不可变绘制内容，含实际文字、粒子、内存图和视频当前帧；新侧是继续运行的普通舞台。旧内容保留目标自己的变换、裁剪、遮罩和组效果，使用目标父坐标系；祖先合成在外层应用一次。两侧在共同组/目标规划中以线性预乘色 mix(old,new,coverage) 合成，复用 mask 表达式、资源上传、绘制管线和 GPU 完成退休。目标使用 multiply/screen/add 时，两路先继承同一当前父背景再各自绘制，混合结果替换父目标，保留与背景的混合语义；不能把透明底上的结果简单 source-over 当作等价实现。转场的零/一端点强制完整旧/新画面；曲线采样夹在 0..1，中途弹性超调不提前结束转场。中间进度沿普通 mask 语义：字符串由 progress 推进阈值，表达式须显式使用 mask.param("progress") 表达变化，mask.gray 本身仍只是灰度采样。冻结画面没有输入、任务或可变节点。正常转场使用 CPU 绘制描述及已有资源租约，GPU 读回与 PNG 编解码仍只属于截图服务。

owner 缺省继承当前作用域/任务。owner 或局部 target 退休、任务取消均撤销转场，清理原 native 续体及保留内容。change 前取消保持原状态；执行后取消显露当前舞台，保留已经发生的业务写入。回调故障清理后原样传播；节点 owner 撤销通过既有 native 回调撤销边界在外围报告 TransitionCancelled，整任务取消仍按原规则。阶段、原续体、返回值、目标/owner 和逻辑计时进入快照，恢复不重跑 change。短暂旧图提取沿保存准备屏障完成；播放中保存共享不可变画面描述，恢复先准备其资源再交换会话。

共享 Rust 接线：reel-ui 的 `RetainBuild::new(&RenderFrame, Option<WidgetId>, BuildLimits, &BudgetPool)`、step、finish 产出不含 CandidateGuard/输入几何的 `RetainedScene`；`RetainedScene::snapshot(describe)` 返回共享的 `SavedRetainedScene`，普通资产以固定 ResourceVersion 与 Image/Font 种类保存，动态内存图和视频保存已有 CPU 内容。player 提供资产来源分类；恢复通过 assets/restore_begin/bind_asset/step/finish 重载并核对固定版本。SessionSnapshot 独立持有这些共享画面供后台编码，adapter Record 仅保存编号。`FrameBuild::set_transitions(Vec<FrameTransition>)` 在构造前接收目标、旧画面、进度与普通 PaintImageMask，产出共同帧中的平衡合成子流。FrameBuild/RetainBuild 记录真实子树边界，不能用 owner 过滤或矩形截图代替。

保存描述的遍历工作区、索引和数组扩容在分配前准入，工作区在描述完成后退款。`SavedRetainedScene::retained_size_steps()` 分项估量保存工作实际保留的描述与 CPU 资源，SessionCapture 按 SnapshotBudget 将其计入 SnapshotProgress.bytes，由原保存大小上限和队列额度准入；普通资产虽然只编码版本引用，现场描述继续持有的图像/字体内存仍计入。每份描述内部去重，共时多份描述之间按保守总量计账。

运行时命令存储（已实现）：普通帧与遮罩使用连续 `Vec<FrameCommand>`，后端读取 `&[FrameCommand]`，不为每条命令创建 Arc。只有实际 RetainBuild 提取目标时，复制目标范围及可达遮罩的命令载荷；图片、字体与视频像素继续共享资源租约。整份独立 FrameDrawing、嵌套旧图和保存描述仍共享不可变所有权，提取完成即释放来源整帧，未选中兄弟的命令与资源不继续存活。复制按命令预付载荷字节与工作量；大型单条路径/字形命令通过 minimum_bytes/minimum_units 请求足够的单步额度，额度不足时尚不复制，不能把该最小额度误称为固定时间上限。取消释放已复制部分；恢复沿同一连续表示和现有资源重映射。范围仅 UI frame/retained/archive、共同 renderer 消费和相应测试文档；用既有 r01_measurements 的矩形/长路径负载比较普通帧运行时分配、录制/编译，并单列 retain 的复制耗时、源与目标重叠峰值和完成后存活量。复跑：`cargo test --release --locked -p reel-ui --test r01_measurements -- --ignored --nocapture --test-threads=1`；行为检查沿 `reel-ui --test r01`、`reel-ui --lib frame::archive`、`reel-render --features wgpu --test gpu_pixels retained_transition -- --ignored` 及原作者转场源码/ASB 检查。

绘制序列化使用 peniko 的 serde 功能和 serde 的共享引用支持。绘制提取和恢复按 StepBudget 推进；旧命令、资源引用、动态资源、两侧合成目标及保存暂存均纳入现有预算，最后租约与 GPU 使用结束后退款。普通帧使用连续命令，独立保留画面以 FrameDrawing 共享不可变内容；无转场帧沿原绘制路径，转场进度变化复用旧绘制、图片上传和遮罩程序，不重排旧文字或复制旧像素。设备丢失后的恢复仍需实机验收。可运行示例为 [author_transition.aside](../examples/author_transition/src/main.aside)。

作者窗口在首个已挂载舞台投影及其字体、图片准备好后才提交首帧；作品直接正常结束时也允许呈现其最终空舞台。支持隐藏窗口的平台在首帧实际接受后显示窗口，Wayland 沿首次提交映射窗口的原生规则；不以窗口可见性查询驱动渲染。启动仍由有界 VM 执行片和资源完成通知推进，隐藏窗口不能依赖原生重绘事件才能完成首帧。显示后沿既有按需呈现、旧帧保留、输入验收与设备恢复路径。

共享舞台文字属性 `text_fit` 默认 `none`；`shrink` 按完整、不自动折行的文本与可用宽度等比缩小，保留显式换行，最高为作者字号，最低为 `min_font_size`（默认 0）。富文本字号和 ruby 一起缩放，打字机揭示不改变适配结果；最低字号仍不足时允许溢出，裁剪由节点 clip 决定。布局缓存区分字体缩放与换行宽度，输入链接和无障碍几何使用同一布局。`text_outline` 默认 0，表示字形向外描边的逻辑宽度；`text_outline_color` 默认黑色。描边复用字形 stroke，先描边再填色，并服从完整 cluster 揭示。`text_shadow_x/y` 是有限的逻辑偏移，默认均为 0，双零关闭阴影；`text_shadow_color` 默认黑色。阴影在正文之前绘制，统一使用阴影色，并沿用正文的排版、硬揭示和逐字透明度；偏移与颜色更新复用已排版字形。文字属性应用于 text 与控件标签，输入编辑器保持自己的布局语义。

字体匹配请求合成粗体时，共同文字绘制将每个字形 run 的 synthesis.embolden 传入已有 font_embolden 绘制字段；横纵扩张均为该 run 字号的 1/24，随既有变换和 GPU 栅格密度缩放。真实粗体字面或变量字体轴由字体匹配优先选择，未请求合成的 run 保持零扩张。完整文字、逐字揭示、描边与填色使用同一扩张量，保留整形的字形 ID、推进宽度、位置和可见 cluster；字形缓存沿既有 embolden 键区分。

共同布尔属性 `text_wrap` 默认 true，只控制 text 节点的自动折行；false 保留显式换行，并允许各行超出可用宽度，裁剪仍由 clip 决定。控件标签原有的不自动折行行为和文本编辑器保持不变。构造、直接赋值及 bind/derive 共用节点属性 schema；切换只更新同一 Label 的 LineBreaking 属性并使布局失效，不复制文本或控件。`text_fit:"shrink"` 继续按不自动折行的完整文本适配，不受 text_wrap 开关影响；outline/reveal 沿已有绘制及字形缓存路径。逐字显示复用字形布局；描边宽度变化更新墨迹边界。

`spawn(work, owner: null)` 在普通剧情或回调中启动独立的零参数脚本任务，复用同一 VM、协作调度和执行片。返回对象可 await 并用 `.cancel()` 取消实际生产者；返回值保持身份，多次等待共享终态，错误保留原始故障和源码因果链。省略 owner 时沿用当前回调/任务的节点，主剧情中归会话；节点退休取消所属任务，隐藏不取消。等待者取消不影响生产者。取消请求立即阻止后续脚本执行，清理完成后 await 才得到 Cancelled；取消不执行脚本 finally，与既有 owner 撤销一致。任务启动后先返回句柄，正文在调用者等待或返回后调度；不能通过 spawn 绕过初始化/纯派生/NoWait 边界。任务无主剧情对白/选项权限。任务结果复用 completion 的订阅与终态存储，活动仍用现有计时器，没有空闲轮询或 OS 线程。

VM 任务槽按并发数量增长和复用，每次逻辑任务准入都检查连续执行空间，包含复用任务槽与派生值重算。连续空闲段不足时沿 FrameSlots 整理/扩容，保留稳定根身份，搬迁双份驻留计入堆预算并遵守宿主的容量上限；默认无固定任务数量配额，宿主可显式设限。任务增长失败释放部分根，不影响已有任务。

任务共用的执行空间在实际调用入栈前按整组窗口再次准入。调用原有的工作/分配预算包含本次必要的整理或扩容，预算不足时保持调用续体并让出；源参数借用和窗口发布在准入成功后发生。脚本/native 调用、默认参数、类初始化、排序比较器、native 回调及 finally 根使用同一路径，覆盖已启动任务互相消耗余量和单片深调用。窗口搬迁保留根身份，当前 VM/SessionSnapshot 格式沿用原版本。

Reel 会话调度显式选择 `SessionBudget::Exact(SliceBudget)` 或 `RetryMinimum(SliceBudget)`。Exact 严格遵守传入额度；播放器使用 RetryMinimum，为主任务、回调、派生和取消清理分别保留 VM 报告的最小工作/分配需求，仅在该任务下一片将对应额度提高到实际需求。指令数、截止时间及 Heap 硬配额保持原值；截止时间导致的零进度让出保留未消费需求，真实推进且无新需求或任务终止后清空。提示不进入快照，恢复后的 VM 可重新报告。该策略不按失败次数增长额度，也不永久扩大普通片预算；一次不可分窗口搬迁仍可能超过窗口目标时间，须报告实际单片耗时，不能视为硬实时保证。

原生续体使用 `NativeBudget::Exact(work)` 或 `RetryMinimum(work)`。Exact 保留额度并报告下一步的最小需求；RetryMinimum 从当前调用自身的续体取得需求，只扩大这一步的工作额度。属性配置分片准备最终提案，在提交前核对相关属性版本，并一次发布全部写入；其他任务可在准备期间继续运行。

`tween` 的 easing 另支持 `quad_in`、`quad_out`、`quad_in_out`，分别为二次加速、减速和先加速后减速；既有 in/out/in_out 为三次曲线。曲线枚举存入动作描述并由活动采样，不逐帧调用脚本。

正弦曲线 `sine_in`、`sine_out`、`sine_in_out` 分别为 1−cos(πt/2)、sin(πt/2)、(1−cos(πt))/2；in 从慢到快，out 从快到慢，in_out 两端慢、中间快。t 为已归一化的时间，起止精确为 0/1。沿现有活动采样、所有权与取消执行，不增加脚本回调或逐帧分配。验证三条曲线的源码/ASB、起止点、中点及中间时刻，并检查采样不增加 VM 载荷块。

对白的句内时序属于共享 Text 数据，与视觉样式分别存储。`rich` 支持成对的 `[speed=15]`（字素/秒）、`[speed=2x]`（玩家速度倍数）、`[speed=0]`（即时段），以及独立的 `[pause=0.5]`（秒）和 `[next]`（本组全部行均指定时自动接下一句）。结束 speed 恢复外层速度；节奏位置按完整 Unicode 字素边界执行。普通 text 和历史显示完整正文，不执行时序。全局速度 0、手动补全与快进越过剩余时序；暂停、失焦、模态和资源等待冻结剩余时间。自动续句须等待完整正文被接受呈现，尾部停顿也须完成，不使用全局自动阅读延迟。时钟推进按跨过的标记段计算，不逐字符分配或重新排版；等待按下一个字素或停顿截止时间调度。

`game.exit()` 请求结束当前作品，不调用进程退出或持有平台句柄。请求一经接收便不再执行脚本；宿主沿既有关闭路径取消任务、活动和资源，增量完成清理后关闭作品窗口。嵌入宿主可读取 SessionVm::exit_requested，并决定其外层应用如何处理。验证主剧情和 UI 回调退出、未决等待清理及实际窗口关闭。

普通 UI 流程使用 `completion(owner: null)` 创建一次性结果，`await result` 等待，回调用 `result.resolve(value: null)`、`result.reject(error)` 或 `result.cancel()` 结算。owner 省略时沿用活动规则：主剧情归会话，回调归事件节点；显式节点退休使未结算结果取消。第一次结算返回 true，之后返回 false；null 也是合法结果。重复等待读取同一终态，resolve 原样返回值，不采用另一个可等待对象。结果不占用对白/选择权限，不记录剧情历史；组件可以返回 `{root, result}` 供主剧情或其他回调组合。

`completion` 和 `spawn` 的结果提供只读可观察 bool `pending`：创建时为 true，返回、拒绝、故障或取消清理完成时变为 false；任务取消清理期间保持 true。支持普通读取、bind 与 derive，不允许脚本赋值。等待者取消只解除订阅；completion 归当前 scope 时，scope 退出取消该生产者并发布状态，长期组件可据此判断重入，无需维护独立的 open 标志。拥有节点/作用域的 completion 用有预算的弱登记追踪；未被 await 的结果也在所有者变化后的维护阶段发布取消，随后响应式求值稳定再呈现。注册在验证 owner 后追加一条弱登记；正常结算、所有者变化及 GC 周期完成时触发分片扫描，每步最多 64 项。登记不保活结果及其 owner，稳定帧不扫描，退出释放已有存储。登记和可观察值随快照恢复。

Rust 宿主内部弱登记复用 `Heap::peek_weakref`，在不运行 GC 的事务内检查目标，不给当前标记周期保活；需跨事务持有值时使用 `upgrade_weakref`。属性订阅采用相同窄入口。

此接口属于 Reel 的宿主 Awaitable，复用 Aside NativeFrame、WaitId、错误传播与任务取消；结果、owner 和任意返回值在 VM 堆中追踪，终态释放 owner；服务只保存尚未交付的等待订阅，取消等待者不取消结果。订阅按登记顺序分批维护，每步最多检查 64 项，一轮完成后才调度脚本，避免结算顺序随切片变化。空闲不启动计时轮询，未被引用且未被等待的结果可直接 GC。

工程从 src/main.aside 的 main 启动。main 中的 `game.configure(title: null, width: null, height: null, fonts: null, text_speed: null)` 更新会话的呈现设置；省略项保留当前设置，参数按普通 Aside 表达式求值。设置以不可变值发布给宿主，宿主只在值身份变化时应用。画布采用 contain 变换并共用绘制/输入几何；未设置画布时沿用视口。设置首个画布维度时，另一维默认 1920×1080 的对应值。fonts 接受一个字体文件或非空有序数组；作品须在首次呈现前显式配置字体，缺少字体时报错。节点 font 使用已提供字体中的字体族/回退列表，system-ui 指向配置的默认列表。文件相对资源根解析并共享计量后的数据，字体回退和文字整形复用 Fontique/Parley；作者播放器关闭系统字体发现。平台窗口句柄始终留在适配器。ASB 元数据 schema v1 只保存代码图与入口，运行设置由脚本执行产生。

动作是不可变、可复用的值。`action.then(other)` 顺序组合，`action.together(other)` 并行组合，`action.repeat(times: null)` 重复整段，`action.run(owner: null, paused: false)` 启动独立活动；链式操作每次组合整个前缀，并共享原描述。`sequence(...actions)`、`parallel(...actions)` 用于多项组合。空组合与零次重复无写入地立即完成；持续循环要求单轮时长大于零，包含无限分支的组合在 finish 修改状态前报错。run 在接管前完整校验并行分支的写集合，顺序段到激活时才读取起值与取得属性。活动的 pause/resume/stop/finish 方法返回原活动；暂停、停止、失败、目标或 owner 退休均按整个根活动结算。

每次 run 在 VM 堆中构造可追踪的执行图；同一描述的多个出现位置拥有独立进度，重复次数不展开成节点。构造、公共祖先冲突检查、时间推进、控制和终态清理使用显式游标，按图节点/祖先计工作额度，不递归占用宿主栈。运行时按截止时间处理跨段事件，同一时刻先结束已有段再激活后续段；循环复用起值、版本和计数槽，不分配新 VM 载荷。时间轮结束前保留上一完整舞台投影，脚本任务边界也触发维护，以处理暂停期间的赋值与退休。终态清除描述、目标和执行图强引用；等待者取消不取消生产者。演出示例见 [author_activities.aside](../examples/author_activities/src/main.aside)。

有限动画使用共同节点数值属性，`tween(node, x: ..., duration: ..., easing: "linear")` 描述动作，`run(action, owner: node)` 启动；owner 省略时优先使用当前 scope，作用域外的主剧情归会话、按钮回调归事件节点。`pause(activity)`、`resume(activity)`、`stop(activity)`、`finish(activity)` 控制同一活动；await 返回字符串 `Completed` 或 `Stopped`，目标或 owner 退休产生 Cancelled。

节点共享固定数值 schema，camera 另有取景数值 schema；tween 从这些属性描述生成普通命名参数，取景参数只接受 camera 目标；未知属性由现有编译器/VM 拒绝。PropertyBagTail 仍是自定义 schema 的后续语言能力。终值 null 表示该属性不参与动画。duration 为有限非负秒数，零时长立即提交终值。easing 支持 linear、in/out/in_out 三次曲线、quad_in/quad_out/quad_in_out 二次曲线以及 sine_in/sine_out/sine_in_out 正弦曲线。

`tween(node, from: {opacity: 0}, opacity: 1, duration: 0.5)` 将显式起值与终值保存在同一可复用动作描述中。from 默认为 null，也接受运行时构造的记录；每个键须对应本次提供了非 null 终值的可动画属性，值为符合目标属性约束的有限数值。构造时校验并复制字段值，之后修改记录不改变动作。未列出的起值在该段激活时读取；width/height 可从有限的显式起值启动，否则要求激活时已有有限数值。

段在延迟之后、每轮重复及初始暂停后的首次 resume 时应用 from。run 的准备阶段保留目标当前值和属性产权；段激活先校验全部起终值与当前绑定，再沿同一接管流程取得属性并提交起值。零时长和 finish 直接提交终值；stop、外部写入、owner 退休与 scope 清理沿现有活动语义。执行图仍以固定 Track 槽保存当前起值、终值和产权，原始 from 保存在动作描述，重复复用同一图。每次采样继续直接读取 Track；新增工作发生在构造和段激活，稳态采样保持现有分配成本。

动作目标记录保存 `(field, explicit_start_or_null, end)`，SessionSnapshot 保存该结构，VM/ASB 格式沿用。

活动的写入所有权调整为接管语义：新活动在真正启动属性段时停止与它冲突的旧根活动，之后读取当前起值。暂停保持属性占用，但不阻止新演出接管。直接赋值（包括赋相同的值）停止写该属性的根活动；其他属性的赋值不影响它。退休、取消、停止、完成均只结算一次；等待者取消不取消生产者。终态保存在脚本持有的活动对象中，活跃根及订阅随结束释放，不积累历史活动索引。

库宿主通过显式单调逻辑秒数推进活动；时间片计时与剧情时间分离。推进按调用者给定的活动工作额度分片，同一轮固定采样时间，未完成时保留上一完整呈现；该时间批次触发的派生也须先稳定并提交舞台，才启动下一轮时间采样。持续对白揭示使用相同边界：活动推进与本次叙事投影共享开始时的准入决定；sync_stage 仅提交已经稳定的属性，不重新采样。派生内部调用 game.dialogue 等 getter 读取本批投影，首次创建时初始化。显式叙事命令和内容切换仍发布实际写入，揭示期间的新值合并到下一批，完整文本与输入推进沿原协议交付。派生跨片期间仅更新已有 observed_time 水位，后续采样直接使用最新时间，不重放被合并的周期；已有活动采样轮仍按固定 target_time 完成。宿主继续驱动相同的最终时间即可排空最后一批，停止活动保留最后已发布值。私有 Host payload 的调度链、候选依赖和继续状态只维护 GC 写屏障与内容版本，公开属性发布才通知对应派生；属性占用直接索引对应活动，启动接管和终态摘链不扫描历史活动。窗口只在有运行中的动画时请求后续时间更新，暂停和纯输入等待可休眠。动画直接写现有节点字段，变换沿现有 compose 路径，不逐帧分配脚本对象或重做文字布局。验收检查源码/ASB、终态/取消/接管、时钟和画面投影，记录稳定推进期间堆分配与布局次数；所有动作、活动、属性写入版本与占用关系都位于既有 VM 堆；单一活动链根保活生产者，不设活动数量上限。会话适配器已调度主任务与按钮回调，活动等待维护多个独立消费者；取消消费者不取消共享生产者，订阅随真实等待失效或交付释放。可运行示例为 [author_activities.aside](../examples/author_activities/src/main.aside) 和 [author_buttons.aside](../examples/author_buttons/src/main.aside)。sequence/parallel/repeat、scope 活动所有权和统一执行快照已贯通同一协议。
作者能够以符合直觉的接口表达、组合和改变想象中的画面与交互，是实现和设计取舍的共同标准。可运行的小场景用于验证表达能力，不限定引擎最终范围。实现过程中发现不自然的接口，应同步改进规范和作者指南，不为维持尚未发布的内部接口引入兼容层。

`delay(seconds)` 描述有限非负时长的等待，与 tween 共用 run、sequence、parallel、repeat、暂停、停止、完成和 owner 取消协议；等待不拥有舞台属性，也不需要虚拟节点。`await run(delay(1))` 等待一秒，零时长立即完成。纯等待按最近截止时间唤醒；存在属性动画时沿正常呈现节奏采样，暂停和平台挂起不消耗等待时长。快进沿既有有限会话活动规则处理。

桌面播放器的初始客户区默认跟随作品首次配置的逻辑画布尺寸；未配置画布时使用 1280×720。Rust `StoryWindowOptions.logical_size: Option<[u32; 2]>` 可以显式覆盖窗口尺寸，`canvas_size` 仍只定义逻辑画布。首个画布应用后不再自动调整窗口，玩家主动缩放与后续画布变更继续沿 contain 绘制/输入变换处理。该策略仅属于窗口适配器。

宿主对象退休时，依赖它的 derive 解除全部输入订阅并冻结在最后一次成功的值，已排队的计算同时失效；直接访问退休对象仍报 RetiredHost。移除最后一个属性消费者会递归解除其派生输入订阅，保留的派生引用再次绑定时恢复更新；仍有共享消费者的计算保持活跃，已退休输入的冻结值不再恢复计算。断开沿实际订阅边迭代执行，不扫描整个堆；测试覆盖条件外观、共享与重新绑定、排队后退休、GC 与源码/ASB。

派生读取的一致性：同步作者脚本段结束后，普通 observer 继续沿现有队列调度；在派生或组件工厂实际读取可观察属性时，绑定最终来源若是尚未稳定的派生输出，先在同一 VM 任务中调用该输出已有的 native/script 续体，完成后重试原读取。外层 PC、寄存器和 TableScan 保留，外层捕获通过既有 Parent 链恢复；消费者不重跑，条件分支首次读取的新上游也遵守此规则。普通脚本段中的读取继续使用最后成功发布值，写入不会在语句之间启动 observer。祖先有效性复用现有派生索引，不在每次 next_derived 排序或复制完整依赖图。读取自身或活动捕获祖先报告 BindingCycle，错误仍定位到作者读取/生产位置。准入沿原 Meter、FrameSlots 与 HeapReservation；从队列认领生产者的工作按现有队列长度计费，取消/故障/GC/中途快照复用同一 native 帧和对象图。VM 快照保存 ReactiveRead 返回位置，SessionSnapshot 直接拒绝旧格式；Aside 字节码/ASB 和作者 native 签名保持，验收包含反序队列、菱形及首次条件依赖、绑定转发、预算让出、原始错误、取消和刷新中保存恢复，并用图库连续打开/切图/返回链执行源码及 ASB。

### 按业务身份复用的作者列表

滚动组合公开 `scroll_x/scroll_y` 可写可绑定位置，以及 `viewport_width/viewport_height/content_width/content_height` 只读可观察尺寸。实际布局将位置钳制到内容范围，滚轮、触摸与焦点跟随回写同一位置；只有改变的数据发布通知。`content_align: "start" | "center" | "end"` 在滚动轴上对齐短内容，默认 start；follow_end 继续保留上滚阅读位置。位置更新只合成，不重新测量文字。滚动位置与虚拟行前缀在合成中保留亚像素精度，父级滚动和平移后的行共用连续坐标，不分别取整；绘制、命中和几何查询使用同一最终变换。`input(on_pointer: callback)` 交付主指针拖动的 `{target,x,y,phase}`，坐标来自已接受呈现的节点局部坐标，phase 为 pressed/moved/released/cancelled；按下捕获，失焦/隐藏/禁用取消，回调归节点所有。同一节点、同一处理器连续到达的 moved，在前一个回调尚未开始时合并为最新坐标；已经开始的回调参数保持稳定，pressed/released/cancelled 不合并。参数由 VM 调用帧持有，快照保留最终坐标而不保存合并优化的临时索引。作者以普通节点和绑定组合轨道、滑块和箭头，不固定滚动条外观。验证拖动及模态取消、双向位置绑定、短内容对齐、追加跟随及连续滚动期间布局次数不增长。

key 接受任意 Aside 值，标量保留类型、字符串按内容、其他对象按完整堆身份比较；浮点正负零为同一 key，NaN 使用稳定的位模式顺序。不调用作者比较器或对象钩子。引擎返回的不透明选项 id 可直接作 key；列表保留原键的强引用，GC 与槽复用不改变行身份。

隐藏滚动区域继续保留作者请求的位置与上次有效尺寸。恢复后重新创建的隐藏控件尚未布局时，合成阶段保留待应用偏移，并停止向 VM 发布滚动与虚拟列表几何反馈；重新显示并完成布局后，再按实际范围应用位置。复跑 `cargo test --locked -p reel-player --lib hidden_scroll`，并用源码/ASB 检查菜单中保存恢复的长诗页。

来源函数与 key 函数沿既有 NoWait/reactive/native continuation 执行；每项构造可让出，候选数组、行与节点由 VM 根追踪。行工厂通过 VM 的 untracked capture 隔离外围 view 依赖，嵌套 derive/list 仍独立记录读取，故障和取消沿 NativeFrame 恢复上层捕获。稳定行公开只读可观察 key/value/index，保留 key 时只更新 value/index，局部订阅驱动外观。节点复用同一 Stage；子节点批量协调先验证单父、重复与环，再重排保留身份并退休移除的树。已呈现输入仍按实际接受帧和节点寿命判断。

结构协调使用排序索引和二分查找，O(n log n)，不逐项线性搜索全表；只为新 key 执行组件工厂。普通行字段更新由 bind/derive 完成，列表无每帧扫描。排序及最后的树提交为同步批次；索引临时内存通过 Aside 的 HeapBudgetUsage::reserve 归入同一预算，HeapReservation 析构释放计账，持续状态全在 VM 堆。失败/取消不留下 Rust 侧工作副本或独立根。默认保留全部行实例；virtual:true 的组件寿命与窗口协调见下节。示例见 [author_list.aside](../examples/author_list/src/main.aside)。

#### 虚拟列表（已实现）

`list(items, key:, build:, virtual:false, ...NodeProps)` 仍组合普通 `scroll`。virtual 为构造时固定的组件寿命选择：false 保留全部行；true 只保留视窗及有限缓冲中的行，离屏退休取消节点任务与活动，重新进入时重建组件。需长期保留的编辑值、选中状态和业务数据放在作者 model 中。正在聚焦或捕获指针的行在交互结束前保留。第一版要求列表是有限高度纵向 scroll 的唯一流内容；来源预先过滤，build 返回 Node；要求行的自然高度为正。列表本身保留自然高度，不设置 height/max_height、纵向 stretch 或局部 x/y/anchor/rotation/scale；整体变换放在外层 scroll，行内组件仍使用共同舞台能力。宽度、横向对齐、padding 和 gap 沿普通布局。无二维网格、横向虚拟化或作者测量/逐帧回调。

数据来源与 key 的响应式索引和视窗协调是同一个 list native 的两个 observer。滚动不重跑来源和 key；VM 的 `begin_reactive(ticket, output, pure, continuation)` 提取既有 call_reactive 的元数据、NoWait、捕获与恢复规则，宿主维护不需先执行脚本。窗口组件继续通过原生续体调用 build，所有持久和候选脚本值留在 VM 图中。来源和键索引仍为 O(n) 存储及 O(n log n) 协调，不宣称端到端开页成本与总条目数无关。

reel-ui Stage 只投影当前行、逻辑索引、位置、总估计高度及修订；播放器在既有布局中测量当前行，通过 NodeEvent 反馈列表/scroll 身份、修订、实际约束宽度、可见范围、行高和交互保留行。只接受当前投影的反馈。窗口需求合并为最新值，只保留一个构造候选；远距离拖动直接定位目标范围。自然高度在缓冲剩余不足半个视口时补充到前后各一个视口，避免等到未测行进入视口才构造。未知高度采用当前约束宽度下所有已测行的平均值；高度变化替换原样本，不重复计数。Fenwick 前缀保存已测数量与高度和，宽度 epoch 仍以 O(1) 失效；估算变化无须遍历来源。播放器在同次布局中按真实高度连续放置每个连续逻辑段，保留视口首行的投影位置，上方缓冲反向累计；远处焦点/捕获保留行不跨索引断点拼接。该放置由绘制、命中和几何查询共用，测量反馈校准索引及滚动锚点后不再把已测邻居二次挪位。content_height 在部分行尚未测量时是估计值。首帧、远跳及尚未到达的图片可继续改变自然总范围，不能承诺未构造行的精确高度。SessionSnapshot 保存计账的三字段树节点（epoch/数量/高度和）；窗口缓冲、测量与渲染临时空间仍只随挂载行增长，无全量组件预建，测量或结构变化保留可见 key 及行内偏移，锚点删除时使用相邻行；follow_end 保持末端，新用户或脚本位置写入优先于旧修正。窗口边缘导航先挂载逻辑目标，接受新呈现后才聚焦，旧确认配对不得激活新行。

快照保存来源、键/高度索引、已挂载行、锚点和工厂续体；最后提交的行投影独立于候选索引保存，恢复不会提前发布中途候选。不重跑已生效工厂，不保存 WidgetId 或布局缓存。VM 快照保存并重映射 active capture 的 Value，保留 evaluation 代数；NativeFrame 的根范围和 Continuation 仍由原快照重映射。SessionSnapshot 保存 reactive_ready 并重映射 derived_task，继续现有调度；属性提交 proposal 仍须先结束。恢复仍按会话合同清空悬停、按住与焦点。

验证分别记录来源索引、首屏工厂/节点/文字布局、首次接受帧和内存；固定视窗的 1 千、1 万、10 万行负载中，首屏组件和排版不随总行数增长。覆盖变高富文本、宽度改变、key 增删重排、末端跟随、快速往返拖动、焦点与捕获保留、跨窗口导航、候选失败/取消及中途快照。缓冲区充足时纯滚动和空闲不得增加文字排版或列表脚本工作。默认图库逐帧检查为 `cargo test --locked -p reel-player --lib virtual_list_natural`；10万模型数据生成、索引、首窗、远跳及 VM 计账为 `cargo test --release --locked -p reel-engine --lib virtual_list_natural_height_cost -- --ignored --nocapture`。测量与报告存放 target/reel-artifacts；完整文本及翻译目录仍驻留。行为命令：`cargo test --locked -p reel-engine --lib virtual_list`、`cargo test --locked -p reel-player --lib virtual_list`、`cargo test --locked -p reel-cli --test author_lists --test author_reading`。成本命令：`cargo test --release --locked -p reel-engine --lib virtual_list_first_window_cost_against_full_materialization -- --ignored --nocapture`；引擎命令分离数据生成、来源/key 索引、首窗构造和 GC 后 VM 存活计账，不含设备呈现。设置 `REEL_SESSION_FONT` 后运行 `cargo test --release --locked -p reel-player --lib virtual_list_rich_text -- --ignored --nocapture` 验证实际富文本排版、宽度变化和 CPU 接受帧成本；这不是 GPU/实机验收。长菜单与过滤/重排/键盘选择见 [author_list.aside](../examples/author_list/src/main.aside)，历史见 [author_reading.aside](../examples/author_reading/src/main.aside)。

补窗期间的本地滚动反馈：虚拟窗构造与作者模型提交分开。仍挂载且可见的已提交行完整覆盖最新实际视口时，允许继续录制该已提交控件树的本地滚动/交互几何；此回执固定为 `current=false`。只准入完整行工厂返回、普通派生全部结算、仅余虚拟 Window 输出排队的协作边界；已发布来源的 SourceRevision 须等于已提交 projection 的值，无 scroll_to/导航目标、无其他活动/转场/粒子提交。已挂载属性须先按既有版本同步，Stage/UI 的节点、种类、子序及可见性、启用与模态范围保持同一次提交；scroll 的 offset 和完成的普通布局/外观派生可继续同步。覆盖检查复用当前真实布局及已驻留图片，不生成第二份场景或索引。未结算普通派生、未同步属性、来源删除/重排/同数组内容更新、隐藏/模态变化或覆盖不足继续等待。反馈帧可更新真实控件输入几何，不能完成 stage.presented、geometry、scroll_to 或新的 capture；完整提交且被接受后才兑现这些屏障。完整模型回执 `current=true` 要求派生全部结算。

候选续体保留成功发布的 SourceRevision 数值、构造宽度与目标/导航身份。该数值只在完整来源索引成功发布时递增，涵盖原数组身份保持的内容更新及重排；与滚动 Request 分开，不保活旧来源索引。仅 Request/小位移变化时，若候选仍覆盖最新实际视口并包含当前新增 pin，保留已构造行继续完成；overscan 边界变化本身不撤销构造。出候选覆盖范围、宽度变化、来源失效/重新发布或目标/导航替换与取消时才撤销并重定位；新增 pin 必须并入候选或触发重定位。来源重算先清理旧候选，再复用已挂载行，避免新索引持有随后被退休的候选节点。

完整新行工厂返回后，窗口 Work 保存到同一 VM 状态图，结束本次 observer 并排到现有 FIFO 队尾，让已排队的普通派生先结算；宿主在该边界同步已挂载属性，候选子树继续未挂载，全部行完成后只 reconcile 一次。原 owner 取消、行工厂失败、来源更新及快照恢复继续使用同一候选/已提交图。私有 projection 保存来源修订，状态图保存候选 Work 与约束。

#### 按 key 定位（已实现）

`let row = await items.scroll_to(key, align: "nearest")` 对虚拟 list 返回可取消、可重复等待的结果，完成值为真实行根节点，不自动聚焦或延长行寿命。align 支持 start/center/end/nearest；nearest 在整行可见或大行包含视口时保持位置，否则取较近边缘，最终夹取滚动范围。owner 是列表。每个列表只有一个目标，新定位、用户滚动、脚本位置写入或新导航覆盖旧目标；目标缺失/过滤、列表退休及 `.cancel()` 以 Cancelled 结算，取消等待者不撤销生产请求。重排按 key 追踪。

定位与边缘导航共用目标、窗口需求和候选；key 索引及高度前缀直接定位目标窗口，不构造中间行，不重新执行来源/key。布局返回真实目标高度及宽度修订、位置校准和窗口提交后才安装呈现屏障；下一完整实际接受帧才完成。变化使旧屏障失效并重新安装，录制、拒帧及旧帧不完成。NodeScroll 的 input 标志区分玩家滚动/焦点导航与布局夹取；新输入撤销未决定位，旧布局反馈仍按 requested 位置校验。复用 stage.presented 的 Results 生产根队列及请求序号，允许完成值是 Node，不新增调度器、脚本帧回调或布局估计参数。

视口高度变化和当前 requested 所接受的布局夹取保留定位目标，重新校准并等待新帧；被拒绝的旧布局不改目标预期位置。来源或其上游 derive 已失效、正在重算时，旧屏障不能完成，即使来源索引续体尚未开始或尚未结束。安装屏障时检查派生来源；Results 在既有每片最多 64 个生产结果的轮转内，对准备完成且身份匹配当前定位请求的结果再检查。普通 stage.presented/geometry 即使 owner 是该列表，也保持各自语义；无关列表或派生更新不阻塞稳定结果。Aside 的 freshness 查询不执行脚本或改变调度。已提交依赖保存可重建的派生祖先条目索引；普通 model 字段的写入沿原精确订阅直接置脏，稳定查询不再反复扫描这些端点，仅检查输出及派生祖先的脏状态、正在求值和未完成捕获。bind/解绑改变连接拓扑时使索引失效，包括值未变的重绑；条件求值提交新的依赖载荷，不复用旧索引。拓扑变化或快照恢复后的首次查询从当前强边重建，仍为 O(n)；稳定查询随派生祖先图增长，来源生成、订阅提交及索引也不因此成为常数成本。冷查询、稳定查询、提交和额外计账分别测量，不新增逐帧全列表扫描。独立成本命令为 `cargo test --release --locked -p reel-engine --lib virtual_list_scroll_to_model_key_dependency_cost -- --ignored --nocapture`，分别报告已建好来源后的有效性检查和完整远跳 CPU 路径；数据生成和来源索引不计入这两个时间。

快照保留目标 key、请求身份、结果及构造续体，未决定位恢复后重新测量并等待新接受帧；候选仍不提前发布。验证远跳 10 万行的工厂/节点/文字布局有界且来源/key 不重算，变高/宽度/重排、快速覆盖、删除/退休/取消、旧帧及未测量/构造中/已安装屏障三阶段快照。 行为命令：`cargo test --locked -p reel-engine --lib virtual_list_scroll_to`、`cargo test --locked -p reel-cli --test author_lists`。定位成本命令：`cargo test --release --locked -p reel-engine --lib virtual_list_scroll_to_far_window_is_bounded_without_reindexing -- --nocapture`；设置 `REEL_SESSION_FONT` 后运行 `cargo test --release --locked -p reel-player --lib virtual_scroll_to_far_target_has_bounded_actual_text_layout -- --ignored --nocapture` 验证 1 千/1 万/10 万项的真实文字排版与接受帧 CPU 路径，均不包含 GPU 呈现。交互示例 [author_list.aside](../examples/author_list/src/main.aside) 的 Item 998 按钮演示按业务 key 定位。

### 当前作者舞台实现合同

文字渐变显现采用共同标量属性 `reveal_fade:null|finite`、`reveal_fade_from:byte=0`、`reveal_fade_step:nonnegative finite=0`，在硬reveal范围内按逻辑字素序号k施加 `clamp(reveal_fade-k*step,0,1)`。起点之前保持原透明度；连字与ruby base/注音共享完整范围的最小透明度，文字、描边、强调及线条装饰使用同一绘制系数。指针和键盘沿原完整链接显现规则，并要求每个字素透明度大于0。fade与step沿普通绑定/动画；from沿普通赋值/绑定，保存、取消和owner退休复用节点与活动合同。

Label在启用淡显或正文改变时建立逻辑字素起始byte索引，每个字素保存一个usize（64位宿主8字节）；更新标量只请求重绘，继续使用同一索引和全文布局缓存。当前可见run的cluster投影产生透明度分组，字形位置沿原排版；同透明度连续字形合批，完全不透明段复用原绘制路径。每帧暂存覆盖当前可见字形范围，正文索引随Label寿命持有，绘制记录沿共同帧预算计账。

`input(on_wheel: callback)` 沿普通指针冒泡接收内层控件未处理的整个滚轮事件；首个声明回调的 input 消费该事件，不拆分轴或剩余量。事件为 `{target,x,y,delta_x,delta_y}`：x/y 使用已接受画面的节点内容区局部坐标；delta 使用逻辑窗口轴的像素，正值向右/向下，不按画布或节点缩放/旋转二次转换。LineDelta 每行40像素，PixelDelta 除窗口DPI，PageDelta 乘接收input的内容区宽高，与现有scroll策略一致。内层scroll实际移动时消费，边界未移动才继续冒泡。回调可等待并归节点owner，模态、隐藏、禁用、退休及接受画面沿共同输入合同；没有按住/释放状态。

`stage.capture(width: null, height: null, owner: null)` 返回可取消的 Awaitable，完成值为不可变 PNG Bytes。宿主锁定请求被取走时最后实际呈现的帧；没有已呈现帧则拒绝。省略尺寸使用该帧尺寸，只指定一边则保持比例。截图按目标尺寸绘制，不改变舞台、呈现确认或输入几何。截图后端与窗口系统独立；GPU 读回使用完成通知，PNG 编码在工作线程完成，字节及排队工作计入资源预算。节点退休、取消、会话替换和设备丢失终结请求，旧帧资源保留至 GPU 完成；截图处理中不采集执行快照。`image(source:)` 同时接受资源名和 PNG/JPEG Bytes；相同字节对象的普通属性更新复用图像来源。

`saves.save(slot, title: null, thumbnail: null)` 接受 PNG Bytes，与执行快照在同一个槽文件内原子提交。`saves.list()` 只返回元数据及 has_thumbnail；`await saves.thumbnail(slot)` 按需返回 PNG Bytes 或 null。目录存储将 PNG 作为压缩快照后的可选附件，用固定尾索引定位；列表不解压快照或加载图片。缩略图由作者在打开菜单前获取，不自动截图菜单。Rust 宿主实现 `CaptureBackend` 并驱动 `CaptureService`，在 `StoryRuntime` 显式启用 frame capture；`CaptureService::drive` 接收 `&mut AsideSessionUi` 与最近已接受的 `RenderFrame`，通过 `StoryRuntime::driver_mut()` 取得驱动以遵守执行快照的可变访问屏障。未安装能力或资源不足时请求拒绝。GPU 提交保留资源至完成，取消的 PNG 编码在输出分块处退出。作者示例：[author_capture.aside](../examples/author_capture/src/main.aside)。

对白确认按已呈现的交互、揭示状态和输入范围配对，与帧重绘版本分离。推进光标、立绘动画、悬停和控件反馈不会作废同一次按下/松开；换句、揭示完成、替换舞台根或切换模态范围后，旧手势不能确认新内容。键盘、手柄和指针共用此规则，命中仍使用已接受帧的几何。

普通图片上传先按当前帧（含转场与 mask）的唯一资源检查驻留量和单张上传峰值，再独立分批提交。已提交图片进入同一缓存，准备帧失败不丢弃已完成的上传；暂存、资源租约和提交额度保留到 GPU 完成；图片缓存只弱引用源资源，既有帧租约可继续使用退休版本，缓存不延长 CPU 像素寿命。分批完成只发出 RenderEvent::ResourcesAvailable 唤醒等待，不接受候选帧或更新输入几何。冷缓存恢复不要求所有图片的上传暂存同时驻留；真正超过宿主配置的图片驻留/单张上传峰值在 GPU 分配前拒绝。映射失败返回设备原始错误，不能 panic。纯 CPU 检查预算计划与调度，实际设备上传检查单独标识。

文字 GPU 绘制按字体资源、字体索引、字号、hint、变体坐标和字形 ID 复用字形图集，使用 guillotiere 分配图集矩形；同页连续字形合批，位置和颜色不进入字形缓存键。未提交上传仅在当前 preparation 可见，提交后发布；图集矩形由 prepared/in-flight/cache 共同保活，最后引用释放后才能复用。页面与上传分别计入现有资源预算，压力时回收无使用者缓存，字体退休解除缓存。

单行 text_input 在给定高度内垂直居中，文字、选区、光标、点击及 IME 候选位置使用同一偏移。文本节点的自动高度按其显式宽度换行测量，滚动末端以完整排版高度为准。text_input 与 text_area 共用 text_align 和 line_height；text_area 的 text_wrap 控制软折行并保留显式换行，text_input 固定为单行。编辑器始终取得内容宽度用于对齐，换行模式交给 reel-text；属性变化保留正文、选区与预编辑，稳定值复用布局。

共同节点的 border_width、border_color 与 corner_radius 绘制内描边和圆角背景，默认分别为 0、#ffffff、0。宽度与半径为有限非负逻辑像素，支持构造、with、赋值、绑定和数值动画；沿共同宿主字段保存。描边位于节点矩形内，可用厚度受矩形短边一半限制；padding 决定内容留白，描边和圆角沿用原布局与命中矩形。播放器复用 reel-ui 的边框和背景绘制，样式变化只请求重绘。

容器构造的 children 是候选节点序列：同一序列显式提供完整组件根时，已属于该组件的后代不再作为兄弟重复挂载。根的顺序沿候选顺序；重复身份、退休节点、已挂舞台的组件或缺少完整根的外部子节点仍报错。add 返回值和单一父级所有权不变，尾随块与显式 children 数组行为一致；Aside 通用收集规则不变。Rust Stage::composition_roots 只读归一化候选，create/add 仍严格准入；失败不改现有树。祖先路径在本次构造中记忆化，避免重复遍历共享长链，不增加逐帧工作。

选择控件复用共同舞台：`checkbox(label: null, value: false, on_change: null, ...)` 与 toggle 共用布尔值、双向绑定及按钮激活，仅装饰和可访问性角色不同。`radio(option, label: null, value: null, on_change: null, ...)` 将选项绑定到一个业务值；同组各项的 value 连接同一模型字段，激活时写入 option，重复选择不回调，也不取消选择。option 可更新或绑定，旧选项画面的激活不能选择更新后的 option。比较沿用 list 的业务 key 规则，允许标量、字符串及对象身份；未匹配值表示无项选中。导航使用共同方向/Tab 焦点与确认输入，禁用、模态和退休沿共同生命周期。两者支持 children 与 decorated，自定义外观读取 value/option 和既有交互状态。验收源码/ASB 双向绑定、互斥、动态选项、输入消费和取消；值变化保留控件身份，不重新排版未变文字。

单选组导航（已实现）：radio 的 value 最终连接同一可写属性身份时组成导航组，不增加专用容器或作者参数。当前模态范围内 Tab/Shift-Tab 将整组作为一个停靠点，进入选中的可用项，否则进入树顺序首项；组在 Tab 顺序中的位置取其第一个可用节点；离开时跳过该组其余项。方向键及手柄方向按树顺序前后选择，跳过隐藏/禁用项，列表逻辑边缘优先挂载下一行并等待接受帧，确认没有后续可导航项后才回到组的逻辑首项/末项；虚拟列表首尾循环或进入另一列表时，请求该列表的逻辑端点并等待接受帧，不把当前挂载窗口当成组边界。不扫描来源或预建离屏行；Tab 离开组时不为寻找下一停靠点构造虚拟行；跨窗口访问选项使用方向键。新行沿同一选择提交和 on_change 入口。脚本改值只更新下一次停靠位置，不抢焦点；指针与确认仍可选择任一可用项，重复选择不回调。组身份和选中状态投影到共同 UI，旧帧、当前禁用/隐藏、模态和配置修订仍共同准入。延后选择只由当前导航请求在接受帧校验后生成播放器内部 NavigationSelection，复用 NodeChange 和原回调提交；不会放宽普通控件输入门禁。失焦、新模态、绑定身份变化、节点退休或清除呈现会取消旧请求。绑定身份恢复后重建，焦点与按住状态遵循已有恢复规则。导航工作只随已挂载节点增长，空闲和普通值回显不添加扫描或文字排版。验证命令为 `cargo test --locked -p reel-cli --test author_radio_navigation --test author_selection` 和 `cargo test --locked -p reel-player --lib radio_group`。检查源码/ASB 实际键盘及规范化手柄输入、单一停靠、模态/禁用/隐藏、程序赋值、回调次数、动态绑定/选项、窗口边缘和保存恢复；一万行同组选项按 Tab 离开后节点构造数不增加；逻辑首尾循环只构造端点窗口，多个虚拟列表共享绑定时保持树顺序。

`input(actions: null, on_key: null, ...NodeProps)` 可在现有输入节点订阅逐次键盘事件。on_key 收到 target、code（物理键）、key（逻辑键）、phase（pressed/repeated/released/cancelled）、shift/ctrl/alt/meta；每个按键单独配对，系统重复不丢弃。null actions 表示没有动作映射。聚焦控件优先消费自己的输入，未消费的事件沿祖先输入节点查找动作映射及 on_key；最近的匹配节点消费事件，不广播给兄弟或模态范围之外。文本编辑控件保留全部键盘与 IME，未消费的编辑按键也不进入游戏回调。根输入仍作为无焦点时的兜底。焦点改变、隐藏、禁用、模态替换与节点退休撤销旧按键及动作；事件沿接受帧准入与节点任务所有权交付。宿主注入使用公共 KeyboardEvent，无原生依赖；空闲不轮询。此变更覆盖 reel-ui/stage、reel-player 输入路由与 reel-engine 属性/回调，不增加依赖；验证多键交错、重复、焦点取消和编辑控件隔离。

`text_input(value: "", on_change: null, on_submit: null, ...NodeProps)` 是共同舞台的单行文本编辑节点，value 支持可写 bind。编辑复用 reel-text 的 PlainEditor；提交文本与 IME 预编辑分别管理，预编辑只存在于控件，提交才写回模型。on_change/on_submit 收到 target/value，Enter 提交而不推进剧情；失焦、隐藏和禁用撤销预编辑。外部值替换结束当前预编辑，普通值回显保留光标和选区。输入事件携带编辑前值，避免用旧呈现覆盖脚本已修改的文本。控件按需布局，空闲不定时轮询。字体、颜色、背景、尺寸、变换和输入范围沿共同节点；不规定字符数量上限。作者赋值须为单行字符串；玩家粘贴或输入的换行转换为空格。平台适配器转发公共 IME 事件和候选框区域，系统负责转换与候选词；原生窗口类型不进入 UI 或 Aside。桌面复制、剪切和粘贴由可选适配器复用固定版本 arboard 3.6.1。

`text_area(value: "", on_change: null, on_submit: null, ...NodeProps)` 提供多行编辑，与 text_input 共用 PlainEditor、可写绑定、输入所有权和回调。按宽度自动换行，文字从顶部开始；Enter 插入换行，Ctrl+Enter（macOS 为 Command+Enter）提交，Tab 按通常规则切换焦点。上下方向键移动或扩展选区，Home/End 定位当前显示行，配合提交修饰键定位全文首尾。粘贴和输入的 CRLF/CR 统一为 LF；作者提供的字符串原样保存。滚轮使用公共 ScrollState，光标移动与编辑将光标滚入可见区域；文字、选区、命中与 IME 共享同一偏移。预编辑仍只在控件内，取消、失焦、禁用或作者替换值时撤销。保存恢复保留正文、绑定与回调；选区、预编辑和内部编辑视口沿现有单行控件的临时输入状态规则重建。稳定值与稳定宽度复用排版，不增加定时任务。

手柄输入通过公共设备标识、标准按钮/轴和动作映射接入；宿主可直接注入规范化输入，Gilrs 只属于可选平台适配器。ActionMap 汇总同一动作的多个设备/绑定，公开当前值与 Pressed/Changed/Released/Cancelled；松开一个绑定不会清除仍被另一个绑定保持的动作。断连或重置撤销该来源，重映射用新映射替换并显式取消旧状态。导航另将模拟量转换为带迟滞的方向/确认/返回，重复只由单调时钟与下次截止时间驱动，空闲没有逐帧输入轮询。

UI 的语义 Navigation 事件沿焦点、已呈现几何和 modal 作用域分发；确认的按下/松开必须属于同一仍有效目标，失焦、清空呈现或作用域变化取消配对。标准按钮使用既有 Click 行为，方向优先交给控件，再按已呈现位置移动作用域内焦点；焦点请求沿原滚动协议交付，长剧情选项列表滚入焦点项并在新画面接受前撤销旧点击配对；返回交给当前 UI 消费，不能在窗口层提前关闭应用。剧情确认沿既有推进准入，按钮仍消费输入。键盘保留文本/IME语义，未消费的方向键使用同一导航路径。

聚焦容器包含可用焦点后代时，未消费的下/右方向先进入树序首项，上/左先进入末项，随后沿空间位置导航并跳过当前焦点的祖先；Tab 保留父容器的停靠点。首次方向导航选中的入口为容器时，同样直接进入其可用后代。入口仍受已接受画面、模态、隐藏与禁用状态约束，沿现有导航缓存遍历。input 声明 on_key 或键盘/手柄绑定时参与焦点与无焦点键盘/手柄路由；只处理指针或滚轮的 input 保留指针命中、捕获和回调，按下时保留当前键盘焦点。回调和绑定改变时同步更新焦点能力。

Windows/Linux/macOS 手柄后端复用锁定 Gilrs 0.11.2，独立 gamepad feature 不拉入 Winit/WGPU；内置 window 组合启用它。桌面后台阻塞等待事件，经有界队列和唤醒通知交付，关闭取消接收端且不等待设备线程。平台不支持该后端时仍可注入公共输入，不能把桌面编译或注入事件当成各平台实体设备验证。

作者输入使用共同 `input(actions: {...}) { children }` 节点，按 stack 布局。actions 的键是动作名，每项含必填 bindings 数组和可选 on_press/on_change/on_release/on_cancel。数组中的控制名字符串使用 scale=1、dead_zone=0；需要配置时使用 `binding(control, scale: 1, dead_zone: 0)`。两种写法在同一入口解析物理键、标准手柄按钮/轴和指针按钮，共用 ActionMap 聚合设备及绑定。每项也可用 on_action 接收完整事件流；非 null 的 on_action 与任何非 null 阶段处理函数互斥，冲突在属性提交边界报错。遗漏处理函数仍更新动作保持值和同步消费输入，该阶段不创建 VM 事件对象或回调任务。

`node.value(action)` 返回当前保持值；事件为 target/action/value/phase，phase 为 pressed/changed/released/cancelled。零参处理函数直接调用，一参处理函数收到事件；普通调用可以等待，返回 Task 不自动等待。回调沿节点任务所有权执行。输入节点自身获得焦点后接收键盘和手柄动作；子控件取得焦点会接管输入并取消父输入节点的保持，键盘和手柄保持一致。声明为 modal 的输入子树自动接管焦点。消费发生于同步路由。

赋给 `node.actions` 的新表在舞台同步时整体生效，物理绑定与处理函数作为同一代快照安装，保持值归零；仅换处理函数也建立新代。构造立即安装声明；后续同步脚本段可继续组装待生效的新表。生效后的内部修改不更新映射，后续更改赋一张新表。旧代仍保持的动作只向旧处理函数交付 cancelled，旧释放不能激活新处理函数；已接受旧画面在换代后交付的释放也按旧取消清理。当前映射和至多一份仍等待终止交付的旧映射由输入节点保活；播放器在替换输入投影时排队旧取消，并在继续输入前交付整批事件。未持有的中间映射立即释放，终止回调启动后沿普通任务持根，旧映射不累积。快照保留已安装声明，恢复时保持值归零并释放旧代取消状态。

按钮取得焦点后，原始 South 按下与释放停止向祖先动作映射冒泡，交由宿主 NavigationInput 生成统一确认事件；按钮按配对释放激活。East、方向及其他未消费的控制仍可交给祖先映射；未被映射消费的方向继续空间导航。父 input 已保持的动作在焦点转移时取消，后续释放不会留下保持值。 回调任务退休后，调度游标保持原本的下一存活任务，游标越过末项时回到主任务位置；空闲后的新批次从首项开始。暂停任务继续按既有轮转与 await 边界让出执行，快照保存同一调度位置。
输入节点的 actions 替换沿上述代际合同原子撤销旧保持值。失焦、清除已呈现画面、模态切换、隐藏或禁用撤销动作并发出 cancelled；退休则撤销节点任务并释放状态。取消只清理旧所有权，不产生点击或剧情推进。原生控制事件先路由作者输入；已消费的控制不再变为默认菜单导航，设备断连仍清理双方状态。Rust 宿主通过相同 NodeKind、NodeAction 和公共输入事件使用这一能力。设备身份以 Keyboard、Pointer(PointerInfo)、Gamepad(id) 枚举区分，数字不会跨来源碰撞。原始 Input 事件返回作用域内实际动作消费结果，未绑定控制再经过同一模态内的语义导航。AsideSessionUi 的 presented/clear_presented 返回 Result，交付呈现切换引发的取消；宿主据此唤醒新任务。

指针输入按 `PointerInfo` 的设备、类型与触点身份保持独立捕获及命中路径；不同触点可以同时操作不同控件，终止事件只释放对应触点。事件上下文中的 active/hovered 指当前触点，绘制和普通查询汇总仍在交互的指针。同一个普通控件的拖动由先捕获的触点拥有，额外触点不能接管；窗口失焦、挂起、呈现撤销、禁用、退休和模态切换逐一取消受影响捕获，取消保留原触点身份且只交付一次。触摸结束清除悬停与触点记录，不积累历史触点。路由沿现有已呈现几何，无稳定移动期间分配，也不按触点扫描控件树。PresentationBridge::handle_pointer_event 返回 Result；新触点的路由和记录在分配、分发前计入已有 max_bridge_bytes，预算不足返回 InputBudgetExceeded 并保持既有捕获。终止释放触点路径，容量按实际占用计量，不另设触点数量上限。

Winit 适配器把原生触摸位置、压力和设备/触点身份转为公共 PointerEvent；库宿主可以直接提交该公共事件，不依赖窗口或 GPU。触摸点击沿现有按钮与剧情输入配对，拖动沿原控件行为。此合同不把平台未提供的手势推测为点击。

共同节点的 `modal: false` 属性控制输入作用域，绘制层序仍由节点组合顺序决定。最上层可见 modal 子树独占输入，范围之外及未被子树处理的指针、键盘和可访问性动作都不能继续触发下层 UI 或剧情；禁用仍隔离下层，隐藏或退休释放作用域。Tab/Shift-Tab 在作用域内循环；进入时选择可用焦点，嵌套或替换时保存该作用域的焦点，关闭后恢复仍有效的先前焦点。新作用域撤销外侧指针捕获，按既有协议只交付一次取消。

作用域随实际接受的呈现几何切换；待提交或被拒绝的画面不提前改变输入范围。撤销呈现期间不交付交互，保留焦点恢复身份供下一有效呈现使用。原生 UI 的 Live 几何模式使用同一作用域规则。Rust 控件通过公共上下文设置 modal，Aside 节点映射到同一个字段与输入分发层；回调进入 VM 前仍检查当前节点的有效性及作用域，防止过期点击启动任务。已呈现几何在原有导出遍历中记录作用域；指针事件只检查目标祖先链，不重新扫描整棵树。Live 模式仅在模态、可见性和结构变化时重选作用域。焦点恢复只保留每个存活控件上的身份，不积累历史弹窗记录。VM 点击准入按反向绘制顺序读取当前 visible/modal 字段，找到首个作用域即结束，不复制节点文本或完整属性。

作者按钮实现合同：`button(label: null, on_click: null, children: [], ...)` 使用共同节点属性和子树；省略 label 时由 children 定义外观。button 作为 stack 排列内容，label 是居中的文本内容，子节点按作者顺序叠放。`enabled` 是所有节点共享的可写布尔属性，默认 true，禁用容器会禁用整棵交互子树。构造、属性更新、局部添加和退休沿用同一 Stage，不建立独立按钮节点体系。

按钮复用 reel-ui Button 的指针、焦点、键盘和可访问性激活行为；该行为提取为共享实现，普通 Rust Button 与作者按钮共同调用。接受的 click 通过原有 UI 输入槽交付，包含稳定 NodeId；仅最后接受的呈现和仍可交互的活节点可触发，控件消费的输入不继续推进对白。回调可声明零个参数（闭包、普通函数或绑定方法）以忽略事件，或声明事件参数接收普通事件对象；rest 回调也接收该事件。宿主只对确实没有未绑定参数且没有 rest 的回调省略事件，绑定方法的 receiver 不算事件参数；其他参数/类型检查仍使用普通调用规则，不改变 Aside 调用的严格参数语义。`event.target` 为原节点；on_click 可赋新闭包或 null。回调不在控件分发或 Rust native 栈中执行。

行为检查覆盖源码/ASB、零参数与绑定方法、默认参数/rest、普通调用严格检查，以及嵌入小游戏的暂停、保存恢复、完成和退休取消。

键盘激活由同一个按钮配对 Enter/空格的按下与松开；失焦、窗口失焦、禁用和隐藏取消尚未完成的按键。切换模态作用域后的孤立松开或重复按下不能激活新焦点按钮，激活键的两端均由按钮消费。

会话适配层在显式 await 的首次宿主入口、普通调用真正等待或任务结束后选择下一可运行任务，复用 VM start_closure、NativeFrame、等待、故障与 GC。已完成的可等待对象也结束当前同步段，使其他就绪任务获得运行机会。边界由 HostTicket 的 await_activation 与 Entry 标记识别，不按活动类型或 WAIT 效果推断；同一次 await 的 native continuation 不重复产生边界。同步脚本段连续执行；普通 HostCall 和预算耗尽只让出宿主线程，恢复后仍执行原任务，避免属性读改写在无显式等待处相互覆盖。每次接受的事件创建受 VM 资源预算管理的任务和可复用存储的会话 owner，成功返回后释放交付并退休 owner；没有固定作者事件数量限制，也不累积历史任务。任务可等待活动；活动等待维护多个消费者，消费者取消不取消共享活动，结算保留订阅顺序且用线性压缩移除失效等待。回调中省略 owner 的 run 默认归事件节点，主任务仍默认归会话。节点退休在下一次脚本推进前硬撤销所属回调，取消等待并分片清理任务，不能执行退休后的下一条脚本。关闭会话显式汇总主任务及所有回调的清理义务。narrator/choose 仍由主剧情拥有，回调调用它们报告可捕获的上下文错误，不夺取当前对白。

主入口返回后，已挂载的交互节点、正在运行的活动或回调继续维持会话；没有可运行工作时休眠。全部交互与活动结束后按原自然结束路径保留最后画面。单个回调未捕获的故障保留真实源码和完整 VM 故障交付，并暂停会话，供检查和显式关闭清理。

宿主接口拒绝脚本输入时保留 AdapterError 的具体原因，作为 CoreError 的字符串 details 进入原调用位置的故障。播放器和脚本字符串插值显示这份原因；无字符串详情时显示固定错误说明。成功调用不构造诊断字符串，资源不足错误不再额外分配详情。

`reel-player::story_runtime::StoryRuntime` 在无桌面 feature 时同样可用，供任意宿主复用 VM、活动、GC 和关闭调度。宿主传入单调时钟与执行预算，在输入提交后调用 wake_input，并根据 needs_tick 安排唤醒；不把 Winit 事件循环或 WGPU 表面作为调度前提。当前 window feature 只表示内置 Winit + WGPU 组合，其他组合使用公共输入、PresentationBridge 和 RenderBackend 合同；这些接口不得暴露原生窗口或设备类型。

作者滚动容器：`scroll(axis: "vertical", follow_end: false, ...NodeProps) { children }` 使用共同节点。axis 为 vertical/horizontal/both；横向滚动按行排列，其余按列排列。视口使用显式或可用宽高，滚动轴按自然内容测量，始终裁剪。follow_end 在原本位于末端时跟随追加内容，作者/玩家离开末端后保留当前位置；内容缩短时夹取有效位置。滚动只改变 compose 位移，不重建节点或文字布局。

Stage 的内部投影按舞台实例、节点身份及属性/后代修订复用不可变子树。属性修改仅标记自身和祖先；纯滚动不复制未变化的历史节点、文本或子树引用数组，控件同步跳过同一投影子树。子节点编辑按受影响祖先的直属子节点对账，资源解析只访问含图片或遮罩的分支，并将可见性及资源变化纳入投影身份。此缓存不进入存档，不跨新建或恢复的 Stage 实例复用；输入接受版本、按钮内容失效、隐藏资源释放及呈现确认保持原语义。

滚轮和触摸沿共享 ScrollState，最近可消费的滚动祖先优先。点击类捕获允许父滚动容器在触摸超过 8 个逻辑像素的方向阈值后接管，并先向原控件发送 Cancel；滑条等独占捕获保持原所有者。取消、禁用、退休和模态切换沿既有指针生命周期。焦点请求逐层按实际滚动位移修正目标矩形，支持嵌套容器。默认剧情视图复用相同行为。关闭默认对白界面后，未被控件消费的轻点沿现有会话推进协议；拖动接管会取消该轻点，不推进剧情。滚动位移由 UI 管理，不依赖脚本逐帧更新或节点创建，不触发文字排版；窗口和 GPU 不进入公共滚动接口。

动态子树合同：`parent.add(child, index: null)` 将未挂载、无父节点的子树插入 row/column/stack，默认末尾，显式位置为 0..=子节点数，返回原 child。拒绝成环、重复拥有、越界和向叶节点添加，失败保持原树。`node.remove()` 从父级或舞台移除并退休整棵子树；`container.clear()` 退休全部子节点而保留容器。退休清除脚本私有强边、图片请求和目标活动的有效性；属性读写与重新挂载报告错误。类型共享方法保留接收者身份，各操作在调用时检查所需的存活状态；`node.remove()` 及已提取的 remove 都可重复调用，已退休节点立即完成。

`container.move(child, index: ...)` 调整已有直属子节点在同一父级中的顺序，返回原 child；index 必填，为调整后的零起始位置，范围 0..子节点数（不含末端）。移到原位置无变化。非直属子节点、跨父级、退休或越界在修改前失败；不采用、不退休、不重建节点。复用 Stage::reconcile_children 和既有 WidgetPod 保留路径，节点属性、活动、事件处理、焦点和资源身份不变；row/column 按新顺序布局，stack/camera 按新顺序绘制。每次编辑遍历同级引用并复用既有对账，临时引用存储随同级节点数线性增长，无逐帧重排任务。

窗口调度交替推进 VM 和增量 GC；已失效对象仍有载荷等待释放时，先推进有额度的 GC 片，再恢复脚本分配。GC 片同样服从窗口截止时间，不通过增加堆预算掩盖释放积压，也不在一次调用中强制完成整轮回收。

绑定 native 方法直接进入既有 NativeFrame，接收者是受 GC 跟踪的首参数，具名/默认参数、等待与效果检查复用普通调用。结构修改的工作和临时存储随受影响兄弟/子树增长；呈现端按 NodeId 保留原控件，只建立新增子树、删除退休控件和更新顺序。未改变的文字不重新整形，不用全场景替换模拟局部修改。节点操作已进入统一执行快照，scope 临时子树复用同一退休路径；跨父移动与通用事件订阅继续实现。

Reel 源码编译入口将当前注册的 reel::prelude 接口放入外围宿主名称环境，作者无需逐文件导入。模块声明、局部绑定及显式导入优先；普通 use 与别名可取得被遮蔽的接口。名称绑定来自同一宿主注册描述，check/build/dev 与构建出的 ASB 使用同一链接合同；不向源码注入前缀，诊断保留原 UTF-8 范围与行列位置。Aside 的默认宿主环境为空，不依赖 Reel；该默认由 reel-engine 的 AnalysisOptions.hosts 配置提供，不改变语言、VM 或 Aside 字节码/ASB。

图片节点合同：`image(source, fit: "contain", ...)` 使用共同节点属性，source 为非空文件路径，fit 为 contain/cover/stretch。source、fit 可直接修改；省略宽高时使用图片固有逻辑尺寸，只指定一边时保持固有宽高比，指定两边时由 fit 决定缩放与裁剪。构造及赋值只更新舞台描述，不执行文件 IO；仅挂载子树请求资源。

`image(..., slice: null, slice_scale: 1)` 可将同一图片作为九宫格面板。slice 为统一边距数值，或按左、上、右、下排列的四项数组/tuple；边距使用源图像像素，slice_scale 将其换算为目标逻辑边宽，两者均为有限非负数。null 使用普通 fit；启用后填满节点内容框，角保持比例，边沿单轴拉伸，中心拉伸。`slice_center: false` 跳过中心片，只画八块边框；默认 true，支持直接赋值、绑定、分层图片描述与快照。源边距超过图像尺寸时四边统一按比例收缩；目标框不足以容纳边框时再次统一缩小目标边宽，避免重叠；目标面积为零的切片跳过。源中间跨度为零而目标面积非零时，沿重合边界的源像素坐标恒定采样；两个轴同时重合时采样交点。slice、slice_scale 支持直接赋值、bind/derive、layered_image 描述与执行快照。源必须为完整 PNG/JPEG 文件或编码 Bytes，分块 .reel-image 在资源边界带源码位置拒绝。共同舞台保留 Insets 与比例；UI 对同一个 ImageBrush 绘制至多九个矩形，不生成像素、不增加上传或每片离屏裁剪。编译帧中的图片 brush_transform 统一为目标局部坐标到源像素的采样映射，允许零缩放。普通 Painter 正向矩阵在 FrameBuild 转换；切片绘制通过显式 paint provenance 提供采样映射。SessionSnapshot 的 pictures 保存采用新矩阵语义的 FrameDraw。
image 的既有 color 属性是像素乘色，默认白色；sRGB 颜色先转为线性预乘值，再与图像的线性预乘 RGBA 相乘，color 的 alpha 和节点 opacity 各应用一次。普通图片、分块图片和 layered_image 的各图层描述使用同一规则；容器 color 不继承到子节点。变更 color 只更新绘制颜色，不重新解码、上传或复制纹理，也不增加离屏合成。Image 控件通过 typed PaintCtx 记录每个绘制范围的图像颜色，FrameImage 保留资源句柄和乘数；GPU 通用与矩形批量路径共用已有顶点颜色，同资源不同颜色仍可批量。遮罩定义的采样不受被遮罩内容颜色影响。

`layered_image(layers, ...共同节点属性)`（ABI）将零参数计算函数返回的图层数组组合为普通 stack/image 子树。每项为包含 source 的图片属性表，或表示空槽的 null；数组位置是稳定图层槽，从后向前叠放。依赖变化时重新计算，先校验全部描述，再复用槽内图片节点并仅写入变化的属性；null 隐藏并保留槽，缩短数组退休尾部槽。属性省略时使用 image 默认值，bind/derive 属性值在计算中作为依赖读取。返回节点直接参与舞台布局、动画和所有权；图层描述、依赖与节点引用存入 VM 堆并随现有执行快照恢复，不增加 GPU 合成缓存或新的渲染节点类型。逐槽解析/更新使用 native continuation 并计入工作预算；只在依赖变化时工作，不逐帧重建或合成像素。

player 使用锁定 image crate 的 PNG/JPEG 解码器、已有 Image 控件和 ResourceLease。Assets 共用 AssetIo 与 WorkExecutor，默认组装后台 DirectoryIo 和 ImageExecutor；宿主可通过 with_assets 注入自己的适配器。重复 source 共享不可变像素；加载请求随最新舞台更新，过期结果不发布。新舞台所需图片与配置字体全部就绪后整体发布，其间保留上一完整画面并禁用其剧情输入；后台完成唤醒窗口，不通过连续轮询驱动绘制。visible=false 的子树不发起图片、粒子贴图或遮罩加载；已加载且仍挂载的隐藏资源可复用，出现未缓存的可见资源需求时先驱逐不属于当前需求和上一已发布画面的缓存。隐藏子控件不保留像素引用，缓存沿原有 resident 预算计账，不另设缓存额度；组件卸载释放缓存所有权，已接受帧的租约按原有提交寿命释放。opacity=0 仍准备资源，便于淡入演出。关闭不等待文件 IO/解码，撤销接收端及请求，后台任务持有的内存预算随退出释放。文件读取和解码不进入绘制热路径，不逐帧复制像素。

虚拟列表补充缓存行时，图片请求按现有资源队列槽数有界接续，读取与解码共用原有输入、临时、驻留预算；同轮领取完成项，撤销整个过期请求集合。播放器使用同一当前候选布局与输入树：已准备的连续虚拟行覆盖视口、所有实际绘入视口的行子树资源完整时，滚轮和拖动可在离屏行补图期间继续呈现当前画面。布局前要求缺图只属于虚拟行内的普通 image，且 width 与 height 已明确；未知自然尺寸、未齐遮罩或粒子纹理沿完整资源准备门。资源租约按当前已加载来源解析，当前按钮、文本和悬停属性直接呈现。可见性使用真实变换后的子树绘制边界，包含新行、行变换、子节点溢出与裁切；连续覆盖也按同一布局的实测行位置核对。首次显示、新舞台或模态范围、分块图片及转场继续完整帧门控。资源未全齐时接受帧只更新真实输入几何，新的呈现、几何查询和截图请求等候完整候选接受；按钮仍校验当前生命周期及行为版本。复跑 `cargo test --locked -p reel-player --lib virtual_images`；外部实景用 `REEL_SCROLL_ASB`、`REEL_SCROLL_ASSETS`、`REEL_SESSION_FONT` 指定资源，以 `REEL_SCROLL_FRAME_MS=16.666667`（60 Hz）、`8.333333`（120 Hz）或 `1`（压力）运行 `cargo test --locked --release -p reel-player --lib virtual_list_natural_external -- --ignored --nocapture --test-threads=1`。帧间继续推进资源完成与 VM 反馈，记录资源准备时长、期间完整可见帧接受数及最长接受帧间隔。

资源路径默认相对于 dev 的源码文件或 run 的 ASB 文件所在目录；`--assets <directory>` 显式选择根目录，库宿主通过 AssetOptions 配置，并通过 AsideSessionUi::set_image_waker 安装后台完成通知。同一路径在驻留期间代表同一不可变资源版本，退休后重新挂载会重新读取；不监视磁盘文件变化。加载失败结束本次窗口运行，并报告图片路径和该 image 节点构造位置；它属于异步宿主资源错误，属性类型/fit 错误仍在赋值处同步抛出。嵌入 ASB 的源码位置只用于诊断，不决定运行机器的资源目录。

显式取消清空当前作者舞台并撤销图片请求，随后同步发布取消状态；这条路径不再请求已关闭的加载器。自然结束保留最后画面，直到宿主关闭。库宿主与窗口使用同一生命周期。

GPU 回放在一次帧准备内复用已经完成合成的离屏目标；复用前清空，仍在父组、遮罩或交叉淡化中使用的目标不得回收。预算按实际分配的纹理峰值计费，并保留到 GPU 提交完成，不能在记录完绘制命令时提前退还。同级裁剪与分组不再逐项累积整窗纹理；这一内部变更不改变帧格式、采样质量或默认预算。

图片实现遵循 reel-ui 舞台/资源快照、reel-engine 宿主绑定、reel-player 平台组装与 reel-cli 作者工具的模块边界。图片使用可配置的驻留像素预算与单次解码工作预算，无额外作者节点数量上限；当前解码任务独占临时额度，供像素转换、图片元数据和编解码工作区使用，额度预留不分配等大的缓冲。像素所需空间在解码前检查，解码器私有暂存由 image 的 limits 约束，不能声称为逐字节引擎预算。验收覆盖源码/ASB 的真实图片帧、PNG/JPEG、固有尺寸/fit、共享与换源退休、加载错误及关闭后迟到结果；成本检查关注解码次数、共享像素、空闲时无加载/轮询与最终预算释放。

`reel-ui::stage` 持有唯一的作者节点模型，engine 管理脚本值和会话寿命，player 将同一模型投影到既有控件、呈现桥和渲染器。`text`、`image`、`row`、`column`、`stack` 返回具有稳定身份的未挂载节点；`stage.replace(root)` 挂载新根并结束旧子树。普通函数可返回节点，尾随 children、条件和集合循环按语言的收集规则组合子树。可运行源码为 [author_stage.aside](../examples/author_stage/src/main.aside)。

节点构造器的源码声明标记 `ExternalSymbol::must_use_result`，由 Aside 通用语义分析报告实际丢弃结果的 `ASS016`。直接调用、导入及不可变简单别名采用同一声明；结果传给参数、保存到绑定、返回或被 children 收集时合法，`let _ = ...` 明确丢弃。函数末尾的 `text(...);` 因分号丢弃节点，诊断指向该调用；普通函数和动态函数值保留现有调用语义。收集边界沿 lowering 的词法规则：children 内独立语句收集，参数、绑定初值和函数体建立各自的取值环境。分析使用已有工作预算，临时上下文随 HIR 节点数线性增长，离开分析即释放。

节点公开位置 x/y、可选 width/height、opacity、visible、clip、gap、padding、font_size、color 和 background。未指定宽高采用内容布局；位置默认 0，opacity 默认 1，visible 默认 true，clip 默认 false，gap/padding 默认 0，font_size 默认 24，color 默认白色，background 默认透明。row/column 顺序排布，stack 按书写顺序叠放；x/y 是布局位置上的局部偏移。属性赋值修改原节点，既有引用继续指向同一对象。节点身份、属性和子对象引用由真正的宿主对象承载，不能靠复制普通 table 冒充节点。单父关系、节点退休和根对象保活在挂载/会话边界处理。

`node.with(...)` 使用节点已有的可写属性名配置原对象，并返回其具体节点类型。省略的属性保持当前值，显式 null 按对应属性处理，bind/derive 沿共同绑定协议。一次调用按最终配置验证相互关联的属性和绑定目标，实参顺序不影响有效性；验证失败保留原值与原绑定。构造器、直接赋值、批量配置和编辑器提示共用属性 schema。

共同节点 `font_weight` 选择字体族内的字重，默认 400，接受 1..1000 的有限整数或浮点数。构造、赋值和 bind/derive 经同一宿主字段验证；实际变化使 text、button、toggle、radio 和 text_input 重新整形及布局，并保留控件身份。字体家族由 font 指定，具体 face 和可变字体轴沿 reel-text/fontique 匹配；富文本局部粗体样式继续覆盖所在范围的字重。该字段作为文字样式属性保存，恢复后按原字重重新建立布局。

共同节点构造器（含 list/view/layered_image）native ABI 各递增 1，节点宿主字段验证器递增 1；复用现有依赖与字体资源，每节点增加一个数值字段；稳定字重沿现有文字布局缓存，实际改字重时支付重新整形和布局成本。验证同族不同 face、动态绑定/赋值、非法写入保持原值，以及同包快照恢复。运行 `cargo test --locked -p reel-engine -p reel-player --lib font_weight`，使用项目原创的三份同族微型字体检查实际绘制所选 face、局部富文本覆盖和稳定布局缓存。

共同节点 `letter_spacing`、`word_spacing` 默认 0，使用逻辑舞台像素表示附加推进量，接受可表示为有限 float32 的整数或浮点数。player 将两值传入 reel-text 的 LetterSpacing/WordSpacing 样式：每个整形 cluster（含末 cluster）增加 letter_spacing，U+0020/U+00A0 额外增加 word_spacing，负值允许字形重叠。整形、换行、命中和编辑器光标使用相同的推进量。富文本区间及 ruby 注音继承节点间距，注音自身字号减半而逻辑间距保持；text_fit 的缩放统一作用于字体和间距。

构造、直接赋值及 bind/derive 走共同宿主验证；实际变化清除 Label 布局缓存或更新 TextInput 编辑样式，保留控件身份，稳定值复用已有缓存。NodeProps 增加两个 f64 字段，稳定同步沿现有版本与字段比较路径；变化成本为现有整形与布局工作。VM 宿主字段保存间距与绑定，恢复重建投影和布局。共同节点构造器（含 list/view/layered_image）及字段验证器 ABI 各递增 1，SessionSnapshot、VM/ASB。运行 `cargo test --locked -p reel-engine -p reel-player --lib text_spacing`，覆盖源码/ASB属性和恢复、实际 glyph 位置、负间距换行/光标/命中及稳定布局缓存。

共同节点的 `stretch: "none" | "x" | "y" | "both"` 默认 none；所选轴不贡献父节点的内容测量，父节点尺寸确定后沿该轴填满父 padding 内的可用尺寸，并覆盖该轴显式 width/height。x/y 和 anchor 仍是布局完成后的局部变换。row/column 流轴上拉伸的子节点作为覆盖层，不消耗流位置或 gap；仅跨轴拉伸仍参与正常流排。scroll 使用视口内框作为拉伸尺寸，该轴不计入滚动内容范围。父布局一次确定尺寸，不通过脚本回写测量值，不引入下一帧反馈。验收覆盖内容撑高背景、顺序容器覆盖层、滚动范围、动态修改与存档恢复。

节点变换合同：所有基础节点共同提供 rotation、scale_x/scale_y、anchor_x/anchor_y。rotation 使用顺时针角度，默认 0；缩放默认 1，负数翻转，0 可折叠；锚点按节点布局宽高的比例取值，默认左上角 (0,0)，(0.5,0.5) 表示中心。锚点不限制在节点内部。布局位置加 x/y 表示锚点所在的父坐标位置；变换为 T(layout + x/y) × R(rotation) × S(scale_x,scale_y) × T(-anchor × size)。角度、缩放、锚点均要求有限数值；角度不钳制在一圈内。width/height 与容器排布使用变换前尺寸，变换作用于整棵子树、裁剪和呈现几何。奇异变换沿现有几何规则不参与命中；零缩放不退休节点。

实现复用 Kurbo Affine 和 reel-ui 的 compose/呈现几何，不增加第二套变换或命中系统。ComposeCtx 提供基于完成布局尺寸设置子控件变换的接口；x/y、rotation、scale、anchor 和 opacity 更新保留控件身份，不重新执行布局/文字整形。内容、尺寸、padding/gap、字体大小、显隐及裁剪更新按实际影响请求布局。验收覆盖源码/ASB、嵌套矩阵、锚点随尺寸变化、负/零缩放、裁剪/呈现几何、错误位置及实际 GPU 读回，并记录纯合成编辑期间布局次数不增长。

### 二维取景

`camera(camera_x: 0, camera_y: 0, zoom: 1, camera_rotation: 0, ...) { ... }` 是共享舞台的普通叠放容器。camera_x/y 是取景中心的世界坐标，默认世界原点；zoom 为严格正的有限倍率，camera_rotation 为有限顺时针角度。子树的视图矩阵为 T(视口内框中心) × S(zoom) × R(-camera_rotation) × T(-camera_x,-camera_y)，随后乘子节点既有变换；padding 留在视口坐标。节点自身的 x/y、rotation、scale 和 anchor 仍定位整个视口。width/height 定义视口，省略时沿用普通内容布局。clip 默认 true，可显式关闭；裁剪在视口坐标，不随取景移动。子节点采用未缩放的布局尺寸，正常叠放和单父生命周期不变。

四个取景属性支持普通赋值、bind/derive 与 tween；活动按现有接管、取消与 owner 规则执行。取景运动只请求 compose，保留文字布局、图片资源和控件身份；绘制、裁剪与已接受帧输入均消费同一累计 Affine，没有第二套场景图或逐帧脚本回调。验证源码/ASB、非法赋值原位失败、动画接管与退休、旋转裁剪下已接受帧局部坐标以及连续取景期间布局次数不增长。二维实现不意味着透视相机已支持。

窗口入口为 `reel_player::shell::run_story(CompiledStory, StoryWindowOptions)`。选项包含标题、资源来源、locale、可选的原生窗口 logical_size 和逻辑 canvas_size；字体由作品的 game.configure 提供。窗口复用 SessionUi、PresentationBridge 和现有渲染路径，交替推进 VM 与增量 GC 工作片；等待玩家输入时完成当前回收周期后休眠，输入完成后继续。正常剧情结束保留画面；关闭执行任务取消及清理后退出。ResourceBlocked 保留执行状态，允许 GC 后重试，持续阻塞时停止忙等。源码和 ASB 入口共用编译/注册描述及运行宿主。新能力使用已有资源所有权和预算，不再叠加仅为演示设定的行数、选项或节点上限。

作者命令 `reel check`、`reel build` 和 `reel dev` 共用 engine 的宿主注册与源码编译入口。check/build 保持不执行脚本，允许已解析的 Reel 原生接口；ASB v1 保存既有 Aside 字节码原生声明，运行时仍由 SessionVm 的宿主注册提供能力。`reel dev` 运行当前项目源码，`reel run <file.asb>` 直接解码并运行构建产物，不重编译内嵌源码。window feature 提供窗口命令，未编入时说明所需构建命令。源码入口固定为 src/main.aside 的 main，按实际导入加载多源码模块。模块共用一个字节码程序、固定导入和独立的会话全局环境，函数先链接，顶层表达式按依赖/SCC 顺序初始化。配置标题与 contain 画布沿共同 UI 变换处理绘制和输入。发布链路将清单嵌入 ASB，并交付文本包、资源卷及独立播放器。

一次窗口推进在既有 2 ms deadline 内连续处理同步调用与 VM/GC 工作，直到真实等待、终态或时间片耗尽，再同步画面。创建多个组件或同步替换场景不会被拆成每个 native 一次重绘；时间片包含宿主调用耗时。没有 deadline 的库级 tick 保留单片推进语义。

节点组件的父子私有引用双向保活：持有任一节点都保持其未挂载组件有效，GC 不改变仍被作者持有节点的行为。显式替换舞台或撤销会话仍整体退休旧树，并清除全部私有强边。Host 属性通过只读堆的同步纯验证器在原赋值提交前验证，失败从作者赋值位置抛可捕获错误，原属性保持不变；呈现同步只解码并发布已验证值，不承担作者错误的延迟检测。

RenderRootOptions.default_font 是正式默认字体输入，注册其字体族为 SystemUi 默认族，适用于字体实际覆盖的全部文字；它不再使用仅限 Latin 的测试回退。字体资源拥有整份字体文件或集合，FrameGlyphRun 单独携带 face 索引，各个 face 共享同一资源和内存；默认族按文件中的索引顺序选取，不受哈希遍历顺序影响。

剧情按阅读顺序运行，在对话和选择处等待；角色身份、历史记录与显示组件分开。画面和 UI 共用节点、资源、布局、变换、裁剪和效果组。长期存在的节点通过属性更新，避免每次输入重建整个界面。参考 Ren’Py 的交互与屏幕复用经验。[1][2][3]
核心提供存档、设置、历史、对话与选择的数据模型及操作接口。标题页、设置页和存档页由作者组合；项目脚手架只生成配置和入口，不附带一套完整内置 UI。用针对性契约用例验证这些能力，作者文档中的片段用于说明组合方式。
初期使用一个工作区，先落地默认 winit 窗口适配和 wgpu 渲染器，窗口管理与渲染后端可以独立选择。完整目标包含视频、可替换的软件/硬件后端、文字与本地化、可访问性、二维/透视演出及扩展服务；阶段安排按依赖推进。静态子树缓存和增量快照是保持同一行为的优化。现有 ons-rs、krkr-rs 用作结构参考。[4]

### 已接受帧的几何查询

`stage.geometry(node, relative_to: null, owner: null)` 请求下一稳定投影的已接受帧几何。返回只读 `{width,height,bounds:{x,y,width,height},transform:{a,b,c,d,tx,ty},text_end}`；width/height 是节点本身的布局矩形，bounds 是其四角在参考空间中的轴对齐包围矩形，不含子树墨迹、不裁剪。仿射顺序为 inverse(reference.window_transform) * target.window_transform；参考可为同舞台的兄弟浮层或祖先，null 使用逻辑舞台画布。局部原点与 on_pointer 坐标一致，padding 不额外扣除。旋转、镜像、相机和滚动参与同一变换。参考变换不可逆拒绝 `GeometryUnavailable`。

文本节点的 text_end 为完整正文末尾插入光标下端的 `{x,y}`，与 bounds 使用同一参考空间，包含换行、对齐、富文本和 ruby 的实际排版及节点内边距；逐字显示进度不改变该位置。空正文及其他节点返回 null。宿主按请求读取该帧已有的文字布局，将坐标保存在同一几何回执中。

Rust 宿主通过 `SessionUi/AsideSessionUi::record(&mut bridge, root, frame_id)` 一次取得 RecordedSource 与 RecordedProjection；root 显式传入与 sync 相同的 SessionWidget ID，不依赖 sync 的副作用，纯 SessionUi 可录制实际已挂载树；AsideSessionUi::record 返回 Result<Option<(RecordedSource,RecordedProjection)>>，运行中 stage_ready 为 false 时，只有符合虚拟列表本地反馈合同的已提交树可返回 current=false 回执；其余返回 None（Pending），不创建候选或回执。sync 可能因测量反馈重新产生派生工作，record 必须重新检查实际覆盖与同树条件。窗口保留上次 accepted_frame 和对应输入几何，准备状态 Preparing 区别于 GPU Waiting 和永久 Blocked；VM 预算推进后，可完整录制或可录制本地反馈的判据从 false→true 时请求新帧，即使投影水位未改变也不能漏掉唤醒。系统要求重绘时可重绘旧接受帧，不忙循环构建。CPU 宿主遵守同一 Pending 合同；不暴露接受旧 source 再生成回执的入口。回执的水位来自实际 SessionWidget.current_stamp，舞台身份来自它的 StageWidget，尚未 sync 的模型不得给旧控件树盖新水位。只从确切 CandidateKey 的已接受几何结算；候选布局、拒帧、旧帧重绘和过期表面/会话不得完成请求。目标或参考在接受帧中未挂载、隐藏则拒绝 `GeometryUnavailable`，透明且正常布局可测量。owner、目标或参考退休，以及显式 cancel，取消未决结果；不取消其他等待者或暂停动画。复用唯一 Results 呈现生产根队列及 list.scroll_to 的 create/arm/disarm 屏障，节点守卫存 VM payload，终态释放；恢复后未决查询重新等待新接受帧，已完成数字快照保持原值。不新增逐帧脚本绑定、全树布局反馈或几何/纹理副本。仅遍历同一呈现队列的未决请求，按需查找已挂载节点；不会按场景节点数量逐帧反馈。数值回复的 Vec 扩容峰值和候选帧持有期通过 VM HeapReservation 计入宿主堆预算；恢复每条 Presentation 记录时清除未决旧值，不在恢复结尾追加全队列扫描。验证兄弟/祖先坐标、变换、资源等待、隐藏到显现、取消、过期帧与同包恢复。示例：[author_geometry.aside](../examples/author_geometry/src/main.aside)。

普通图片提供 `repeat: "none" | "x" | "y" | "both"`（默认 none）和正数 `tile_scale: 1`。重复轴按源像素乘 tile_scale 铺满节点，非重复轴保留一份同尺度图像；重复模式不应用 fit，与 slice 互斥。`.reel-image` 是按视窗驻留的完整大图，重复模式在其资源类型边界明确拒绝。四角 `corner_colors: null | [左上,右上,右下,左下]` 在节点内容框内按线性预乘 RGBA 双线性插值，再乘 color 和节点透明度；普通图、九宫格与分块图共用该坐标域。重复图通过共享 sampler 和单矩形绘制，四角颜色通过常量参数参与原绘制，不复制像素、不增加离屏通道。

作者播放器只呈现显式挂载的舞台，不自动创建对白/选择控件或复制内置视图投影。game.dialogue()/game.choice() 保持数据与交互服务。Rust 会话诊断的预览由诊断入口显式创建，不参与 Aside 作品设置。

共同节点增加 `max_width/max_height: null` 和 `align_x/align_y: 0`。上限为非负有限逻辑像素，限制自然尺寸和显式尺寸；内容按上限换行，stretch 对应轴仍由父内框决定。对齐值在 [0,1] 内，按父 padding 内剩余空间定位；stack 两轴生效，row/column 只在非流轴生效，scroll 只在非滚动轴生效。x/y 和 anchor 保留独立变换语义。测量与定位复用现有布局遍历，无脚本测量回调、逐帧节点重建或额外资源队列。行为检查覆盖内容伸缩、对齐、换行、滚动范围与快照恢复。

`scroll` 在非滚动轴以可用内框作为测量空间，保留子节点的显式尺寸和 max 上限；只有对应 stretch 轴强制填满。普通与虚拟 list 的有限宽度因此可在纵向 scroll 中按 align_x 对齐。定向验证：`cargo test --locked -p reel-player --lib scroll_cross_axis`；稳定滚动仍复用原布局。

动态 `view` 的稳定容器按 stack 布局其唯一内容，保留两轴对齐。无焦点时，舞台从前至后选择最内层可见、启用且有键盘/手柄处理的 input 作为键盘/手柄默认目标；已聚焦控件、文本编辑和模态边界仍优先。仅修改文字速度或标题不清理已接受输入，字体与画布变更才失效呈现。WGPU 字形栅格按最终变换的最大放大倍率选择不小于 1 的二次幂密度；缓存区分密度，绘制还原逻辑字形尺寸，轮廓、描边和合成粗体一起缩放。动画同档复用字形，沿既有图集预算和回收，无逐帧全量重排。检查覆盖动态居中、嵌套输入、焦点丢失和 CPU 字形栅格尺寸；设备画质由目标平台确认。
按钮接受状态按节点及其内容子树管理；其他分支的动画或文字更新不使它失效。自身内容替换须先呈现，旧按下不能激活新内容；几何变化沿既有呈现命中/捕获规则处理。只读可观察 `focus_visible` 区分导航焦点外观与 `focused` 的输入接收身份：键盘/手柄导航显示焦点，实际指针移动、按下或滚动切回指针外观但保留接收目标；隐藏、禁用和窗口失焦时为 false。它与 hover 可组合，输入模式转换按事件发布，不逐帧执行脚本。

StageGraph 唯一拥有作者可见节点、稳定 NodeId、父子结构、声明属性与寿命。reel-ui 的控件模块负责对应控件的布局、焦点、捕获、输入法、可访问性和临时交互状态；复合控件内部节点保持私有。两者通过 NodeId 到 WidgetId 映射连接。业务值和控件瞬态不得成为两份互相覆盖的状态。
舞台变更由 Stage 的节点编辑与属性提交入口处理，支持创建、移除、挂载、脱离、替换场景、属性修改、绑定和订阅。提交前统一验证身份、单父与无环约束；失败保留原舞台，成功后才释放旧场景。属性存储是唯一权威，同一事务同一属性采用最后一次有效写入，范围约束先执行，随后静默更新投影。属性绑定保留被引用属性的读写权限，slider 写回需要可写属性；只读派生绑定跟踪实际读取的可观察属性；派生函数不得写入或等待，循环依赖给出源码诊断。

节点公开只读可观察 `hover`、`pressed`、`focused`、`disabled`。hover/focused 汇总节点及后代；pressed 汇总指针捕获，并包含按钮自身的键盘/手柄确认按住状态。disabled 反映继承祖先禁用后的有效值。隐藏、禁用、失焦及退休清除交互中的状态；状态在调度业务回调前发布，展示节点可以直接 bind，无需回调或逐帧脚本。`decorated: false` 关闭按钮默认焦点框、开关默认指标及其留白，保留交互和普通 children；作者用文字、图片、背景、变换及绑定组合外观。仅由交互状态驱动的外观更新保留按钮的在途激活；业务内容变化仍要求接受新画面，新手势不得使用未接受的内容。

所有舞台节点提供可赋值的 `on_interaction: null | callback`，默认 null，用于悬停选中、聚焦演出等业务变化。复用 NodeEvent::Interaction：仅五个交互状态中至少一项实际改变才产生回调；先发布整批只读状态，再按事件顺序调度节点拥有的普通任务。事件是不可变记录 `{target, hover, pressed, focused, disabled, focus_visible}`，状态值固定为该次变化，target 仍是原节点；回调可等待，节点退休取消任务，隐藏或禁用产生的清理状态仍可交付。注册或更换回调不合成初始事件。未注册的节点不分配事件对象或任务，不逐帧轮询；纯外观仍使用 bind/derive。回调及等待任务进入既有快照对象图。行为核对点为整批可见性、重复状态静默、不可变事件、等待与退休、动态换绑及存档恢复。

节点内容与交互行为分开。图片可以具备按钮行为，容器可以整体变换；按钮外观读取 hover、pressed、focused、disabled 等状态，滑条提供轨道与滑块槽位。作者组件与内建组件使用同一舞台能力，具体调用形式见 DSL 文档。节点移除注销绑定、订阅并取消所属任务与活动；隐藏保留身份，脱离必须由显式所有者接管。visible=false 隐藏子树并退出命中与焦点；enabled=false 保留画面并禁用子树交互。两者使捕获失效时统一释放捕获；opacity=0 不自动禁用输入。
仿射变换、裁剪和命中使用同一几何，命中经逆变换计算。非线性视觉变形默认采用作者指定的逻辑命中区，扩展映射另行提供。适配必须验证变换后控件的捕获、焦点与命中一致性。
便携的 RenderFrame 与绘制命令定义于 reel-ui::frame，帧资源由 reel-ui::resources 管理，基础资源身份定义于 reel-core；UI 从不反向依赖 render。Mesh、Material 等扩展类型按既定功能范围逐步实现。RenderFrame 包含帧号、画布与输出尺寸、资源引用以及有层次的绘制计划。计划保留 BeginGroup、EndGroup、裁剪、组透明度和效果参数。组内绘制连续，子节点排序受组边界约束；跨组覆盖由显式 overlay 容器表达。组透明度对合成结果应用一次，效果外扩计入离屏范围，再遵守对应裁剪语义。
普通容器直接合成，遮罩与后处理按需使用离屏目标。先实现相邻兼容绘制合批、排版与字形缓存、离屏目标复用；逐字显示复用完整文本排版，只改变显现范围。属性变化区分布局、内容、合成和命中；淡化不使排版失效，资源版本变化传播到依赖缓存。GPU 资源在实际使用结束后释放。低端模式可降低效果分辨率和采样数，逻辑坐标保持一致。

可写绑定的标量写入与 tween 均沿引用链写入最终源 PropertyId，保留绑定。run 解析并保留真实源身份；被动投影只读取结果，外部源写入按统一规则停止根活动。只读派生属性拒绝直接写入，可用独立 override 层作演出。binding replacement/unbind 先停止依赖旧链的活动；unbind 固定当前基础值。override token 拥有独立属性存储，也遵循读写能力与保留规则。

属性连接由独立 VM 的宿主属性存储维护。宿主显式登记可观察字段及权限；前向引用是 GC 强边，反向订阅是弱边。属性传播只遍历受影响的连接，校验期间读取同一候选图，成功后更新缓存与字段版本；失败保留全部旧状态。绑定创建、替换、解绑、来源退休均维护订阅。写入与动画使用最终属性身份，节点字段继续作为舞台投影的增量版本来源。传播工作缓存复用并计入 VM 内存预算，空闲无订阅轮询。Aside 字节码 使用现有 PropertyRef 指令，不引入平台依赖。

## 六 作用域 样式与状态

### 作者活动与窗口时钟

滚动偏移使用同一活动机制：`tween(viewport, scroll_x: 120, scroll_y: 400, duration: 0.5)` 仅接受 scroll 节点，起点来自激活时的当前偏移或 from，目标值在布局时按真实内容范围限制。暂停、停止、节点所有权和执行快照沿用普通 tween。

组合动作的宿主声明先接通普通可变参数：NativeContract 的 rest 名称和 Rust `#[aside(rest)]` 映射为既有 Aside 字节码 array rest。sequence/parallel 等库接口使用这条通路，不新增 Reel 专用编译语法或另一套参数收集器。范围和验收见 Aside host SDK 文档；动作组合运行器共用下面的活动合同。

`tween(..., easing: curve)` 接受 `std::math::curve` 创建的不可变曲线。时间归一化为 0..1，采样值作为起值到终值的插值比例；构造时要求 sample(0)=0、sample(1)=1，允许中途超调，属性约束保持原规则。零时长、正常终点与 finish 精确提交终值。沿用标准库的 O(log n) 曲线采样和 VM 追踪引用，多个动作可共享一条曲线；采样不建立脚本任务或逐帧对象。暂停、取消、属性写入冲突、重复与快照沿现有 tween 协议。

`tween` 另支持 quint_in/quint_out/quint_in_out 五次曲线和 bounce_in/bounce_out/bounce_in_out 回弹曲线。in 加速、out 减速或在终点回弹、in_out 对称组合；采样位于 [0,1] 且起止精确为 0/1。沿既有活动采样、所有权、取消与快照路径，不分配逐帧脚本对象。

`quart_in/out/in_out` 为四次曲线；`elastic_in/out/in_out` 为衰减弹性曲线，允许中途越过起终值，起止仍精确为 0/1。elastic_out 使用 `1 + 2^(-10t) sin((t - 0.075) 2π / 0.3)`，in 为反向，in_out 为两半对称组合。属性自身的合法范围保持原规则，弹性适合位置、旋转和缩放；opacity 等有限区间属性应选不超调曲线。动作描述保存曲线编号，暂停、取消、预算和恢复沿既有活动路径，无新逐帧脚本分配、参数。

`stage.presented(owner: null)` 返回可等待的呈现请求；`await` 在请求之后的舞台投影实际接受呈现后返回 null。派生状态先稳定，字体与图片先完成准备；录制、拒帧和重绘旧帧均不结算请求。多个请求可随同一帧完成，重复等待同一对象共享终态。请求不暂停其他任务、音频或动画，转场组件可先挂载透明的新内容、等待呈现，再开始动画。owner 沿现有活动规则，退休取消未决请求；`.cancel()` 只取消这个请求，等待者取消不取消请求。请求不允许脚本 resolve/reject。无挂载根但显式请求呈现时允许提交空舞台。复用 completion 的 VM 根、订阅及错误路径，帧携带单调请求序号，不为等待保留场景或纹理副本；没有忙等和新线程。Rust 宿主把 `presentation_request()` 固定到稳定投影，在该投影实际接受后调用 `accept_presentation(session, request)`；旧序号不会完成新请求。

`clock(interval: 1, owner: null, paused:false)` 启动可观察的周期活动，复用活动图、期限索引、暂停/恢复/停止、等待和所有者退休。只读 `ticks` 是 int 完整周期数，初值为整数 0，可直接用作索引或 slice 范围；`elapsed` 是 float 周期总秒数，初值为 0.0。仅在周期边界发布，elapsed 等于已完成周期数乘 interval；interval 必须有限且大于 0。长时钟步长合并为最新周期，不逐个补发，也不创建脚本回调。暂停不计时，停止后保留最后发布值；无终点的 clock 不能 finish。快照保存整数 ticks、浮点 elapsed、周期与暂停偏移，恢复后保留类型与下一期限。owner 规则与 run 相同。周期计数超出 int 范围时在发布前报错。参见 examples/author_clock/src/main.aside。
scope 管理任务、活动、等待、临时节点、属性覆盖和加性偏移。独立 Aside 只认识 TaskId 与原生 FrameId：宿主可撤销指定原生回调区域，旧 ticket/wait 立即失效，按现有切片预算清理内层帧，保留外层 catch/finally，再以 CallbackCancelled 恢复该原生续体；整个任务撤销仍优先。清理期间的边界进入 VM 快照。VM 帧释放计入现有切片预算，宿主寿命检查线性访问当前活动作用域；登记容量计入 VM 堆预算，空闲不重建资源。

`scope(body:, owner:null)` 通过 VM 的 CallScript/native continuation 在原 Task 中执行 body，返回 body 的结果，故障保留原身份。body 可接受零参数或一个作用域对象。作用域用普通宿主 owner 对象和 TaskId/FrameId 关联；`s.run(action)` 与 `s.spawn(work)` 显式归该作用域。run、spawn、clock、音视频、粒子、completion 与呈现请求默认继承当前作用域；子任务继续继承其资源 owner，显式 owner 转交生存范围。正常返回/异常退休 owner，后台资源沿现有账本清理，退场动画由 body 显式等待。附加 owner 撤销时，VM 只释放到指定 native 屏障，宿主退休资源后在原 scope 调用点交付 ScopeCancelled；外层主剧情仍可处理。硬撤销跳过区域内脚本 catch/finally，普通 return/throw 按语言规则展开。活动停止保留当前属性值。可运行示例为 [author_scope.aside](../examples/author_scope/src/main.aside)。
`s.cancel()` 首次请求退休该作用域并返回 true，重复请求返回 false。请求保存在同一作用域维护索引及执行快照中；维护阶段沿 owner 退休路径撤销 native region、嵌套作用域、所属任务与活动，在 scope 外交付 ScopeCancelled。自身取消先完成当前 native 响应，再撤销其区域。handle 仍在结束时退休；提前提取的绑定 cancel 方法在结束后幂等返回 false。

`s.add(parent, node, index:null)` 挂载临时子树：要求与 node.add 相同的未挂载节点，返回原 node，index 省略时追加。普通 node.add 保持父节点寿命。注册发生在同一挂载边界，挂载失败不转交所有权；作用域退出按登记逆序移除仍存活的子树，提前 remove、clear、stage.replace 和嵌套作用域不会重复退休。节点拥有的输入任务、活动与呈现等待沿原退休路径结束，已接受输入几何仍由呈现协议更新。登记数组进入现有 VM 对象图，恢复后只继续等待和清理，不重跑构造。登记与退休按实际节点规模处理；子树移除仍使用舞台同步退休，当前不承诺大型临时树单帧完成。
属性覆盖：`let effect = s.override(node, {opacity: 0.4, width: null})` 创建独立可观察 token，记录参数保留显式 null。允许共同节点布局和外观属性，不包含 on_interaction、控件业务值、只读交互状态或 camera/scroll 专属字段；未知名与类型错误在调用处诊断。token 只公开本次提供的属性和 remove 方法，可直接赋值、bind/derive 或作为 tween 的目标。`node.x` 始终读写基础值，`effect.x` 读写本层值；几何、绘制和输入读取有效值。同字段取最后建立且仍存活的层，修改或重绑不提升优先序。基础绑定在遮挡期间继续更新。`effect.remove()`、scope 正常返回/错误/取消以及目标退休移除覆盖，不把旧值写回基础字段；嵌套与非顶层移除沿同一规则。

基础字段与各层使用真实独立 PropertyId，动画只在实际写同一层或同一绑定源时冲突；隐藏层的动画照常计时。目标退休也退休其全部 token，未完成活动沿原退休路径取消。节点 VM payload 保存有序层与预分配的按字段选择槽，创建/移除才维护优先序；清理不创建引用或订阅，不依赖新分配。token 的绑定、目标、选择引用及 scope 登记都在既有 VM 对象图内，强边保活和退休断开遵循原属性协议；无另一调度器。未覆盖节点保留原版本快路径；有覆盖节点同步检查固定共同属性数的选中层字段版本，成本不随隐藏层数量增长。创建/移除按提供字段与现存层数处理，VM 图和宿主版本缓存计入堆预算。恢复重建投影缓存，保留层序、绑定、活动时间和原 scope 续体，不重跑正文。检查基础 derive 更新、显式 null、嵌套和同 scope 多层、非顶层提前移除、独立动画/真实源冲突、节点先退休、取消/故障/恢复及取消无新增分配。可运行示例为 [author_scope.aside](../examples/author_scope/src/main.aside)，Start 覆盖只读透明度、Stop 恢复持续变化的基础绑定。验证命令：`cargo test --locked -p reel-engine scope_override`、`cargo test --locked -p reel-cli --test author_scopes`；release 成本命令为 `cargo test --release --locked -p reel-engine --lib scope_override_cancellation_uses_existing_storage_and_idle_cost_ignores_layer_count -- --nocapture`。
加性偏移：`let shake = s.offset(node, x: 0, y: 0)` 创建固定含 x/y 和 remove 的可观察属性层；初始化参数接受有限数值或既有 bind/derive，并在发布前验证。token 与 override 共用构建、属性验证、动画目标、scope 登记和退休路径。每个轴取当前基础值或最后存活的 override 值，再按建立次序加上全部 offset；偏移在父坐标系中，node.x/y 仍读写基础值，token.x/y 读写本层。普通 run 在作用域内继承 scope，基础动画可在外层继续；独立层可同时运动，实际同 PropertyId 或最终绑定源的写入仍按原冲突规则处理。remove、正常退出、故障、取消及目标退休撤销本层，不保存或回写旧坐标。最终偏移进入现有 NodeProps 变换，绘制、geometry 与命中共用矩阵，行列测量和原布局尺寸保持不变。旋转和缩放继续通过有序变换及组节点表达。

节点 VM payload 分别保存 override 与 offset 层，仍复用同一 scope 拥有列表。覆盖选择槽与全部偏移 token 都由 VM 图保活，宿主只缓存字段版本；无 offset 的节点保持现有路径。稳定同步对 offset 数量线性检查，变化时线性求和，覆盖检查仍为固定共同字段数；创建预留缓存容量，稳定赋值/动画推进及移除不新增逐帧层存储或 VM 分配。缓存和创建期间新旧存储重叠计入 HeapReservation。快照保存全部 token、绑定、活动和作用域续体，恢复重建同一投影缓存。复跑 `cargo test --locked -p reel-engine --lib scope_`、`cargo test --locked -p reel-cli --test author_scopes`，成本命令为 `cargo test --locked --release -p reel-engine --test scope_offsets_allocations -- --nocapture`；检查包含既有遍历/原生分发分配的同负载基线，不将零新增层分配写成整个同步零分配。示例：[author_offsets.aside](../examples/author_offsets/src/main.aside)。
样式复用由普通组件函数、记录与参数完成。组件默认值、变体、显式参数按序合并；显式缺失与 null/false/0 区分，scope 覆盖最高。运行期样式通过可观察属性绑定，布局/绘制失效分类保持一致。引擎提供通用属性和控件槽位，作者决定视觉风格。
model(defaults) 复制默认图并保持其中的别名/环，所有可变内嵌数据对象使用观察变更钩子，读取与结构操作记录对应依赖；后续会话对象赋值保留共享身份并纳入观察，别名修改同样通知。原输入默认图保持独立。对象按可达性保活。规划中的 state(id,defaults) 是会话内的具名状态根，随保存/回滚恢复，新游戏初始化。规划中的 node.state(key,defaults) 属于具体节点实例的状态槽，在节点 key 与槽 key 同时匹配时重载复用；同名不同实例互相独立。组件销毁后保留的普通脚本引用按正常 GC 规则决定对象是否继续存活。
`model` 的数据图复制由原生 continuation 分片执行，工作对象随 NativeFrame 取消释放，不把复制循环塞进单帧。model table 复用普通 table 身份、方法与属性连接图，新字段可以追加；clear 断开旧字段连接并冻结消费者，旧 PropertyRef 不得指向后来同位置的新字段。后续赋入的数据保持共享身份，嵌套 table 同样可 bind。Reel 在这些同步 continuation 之间保持当前任务，复制不会与其它脚本任务交错。

model table 的 `remove(key)` 保留其余字段的稳定 PropertyId；首次结构删除才建立“当前位置到字段身份”和“字段身份到当前位置”的映射，未删除过字段的表继续直接使用位置。删除字段的身份留下退休标记，同名重插获得新身份，clear 释放整套映射。删除按预算复制剩余键值及位置映射，临时副本由原调用帧保活；工作量与当前条目数和已分配字段身份数成正比，数组块增长先计入 VM 预算。提交先冻结并断开被删字段的连接、释放其活动 claim，再从原表接入最新属性元数据与写入版本，保留复制期间建立的同值绑定及活动。位置变化后的读取、写回、派生依赖和动画写入版本使用同一稳定身份。属性退休及反向依赖通知沿既有同步图遍历，提交成本还随受影响连接规模增长。VM 快照保存新映射及未完成 remove 的原续体；恢复重映射对象身份，继续原操作。

派生绑定复用属性连接图：正向依赖保活来源，反向订阅是弱引用，写入只将相关派生加入去重队列。VM 记录实际成员、索引及集合读取；每次成功计算替换条件依赖。会话在同步脚本段结束与舞台发布之间按 VM 预算排空计算，持续占用原有任务调度边界，避免计算期间其它脚本写入。局部临时对象按计算代数识别，不允许改写外部对象；原生回调只能收紧权限。

作者可用固定 tuple 解构遍历系统记录：`for (key, value) in record.entries()`。编译器在建立任一绑定前完成结构检查和元素准备，按普通 VM 指令预算执行；循环捕获保留各轮值，不为解构复制容器。
系统数据通过 `persistent.read(key, default: null)`、`persistent.write(key, value)`、`persistent.delete(key)` 独立读写。它们返回与 saves 相同的可等待请求；缺失记录返回 default，已存的 null/false/0 不视为缺失。write 在调用返回前按预算捕获纯数据，后台发布完成才结算；read 按预算重建独立数据副本。支持 null/bool/int/float、string/Bytes、array/table/tuple，保留图内别名与环；table 只保存自有字段，不保存委托、模型绑定或类方法。实例、可执行闭包、节点和任务须由作者转换为上述数据。文件不含 ASB 身份，业务版本迁移由游戏处理。
系统记录与剧情槽共用有序存储服务，使用独立文件命名空间，存于宿主指定的 save-directory；不出现在 saves.list 中。会话退出前已交给存储服务的写入继续提交；未交付请求取消。读档不重放系统写入，也不覆盖系统文件。read 返回的副本是普通会话数据，会随剧情快照恢复；作者在恢复分支重新读取当前系统设置后应用，不能把副本当作跨会话共享代理。数据捕获、重建和队列均使用现有内存/工作预算，文件 IO 在后台线程执行。
规划中的 state/node.state 调用原子补充缺失默认字段，下一语句可见。InitSafe 的 story.register、state 及组件状态默认补充先登记候选变更，候选读视图可见拟加入字段而活会话不可见，ModuleSet 提交时才一起生效；失败不修改旧注册表。

舞台控件通过 bind 写回模型时，可与 VM 的分片字段查找交错。单键读取、写入、has_own 和属性引用按键结构校验，在完成时操作当前槽；普通 table、model 与宿主字段共用此规则。覆盖字段值保持进行中的查找，增删或重排键报告 ConcurrentModification；复制、展开、删除仍保留完整内容校验。实际读取时登记派生依赖，更新结果沿现有反应批次提交。VM 快照保存查找续体的结构版本，旧格式因版本语义不同而明确拒绝；并校验嵌入的 VM 版本。独立 persistent 系统数据沿原格式保存。

## 七 VM 与会话接口

### 对白推进状态

分段对白沿既有角色、旁白与 say 提交：`wait: false` 立即返回当前行组的 `Speech`，正文照常揭示。`line.append(text, history: true, wait: true)` 在单发言者行组中追加普通或富文本（正文允许换行），等待本段推进后返回原句柄；`wait: false` 追加后立即返回原句柄。`line.revealed()` 等待当前正文及句内暂停完成，供作者接续音效或演出；`line.wait()` 等待手动、自动或快进推进。两者均返回原句柄，继续追加时保留行与历史组身份、已揭示前缀和进行中的句内时序。多发言者行组可以等待，append 因目标不明确而拒绝。尚未等待推进时，输入可补全正文，自动与快进保留作者正在执行的演出。下一次对白或选择提交退休旧句柄，旧句柄操作在调用位置报错。普通角色、旁白与 say 调用默认等待后返回 null；仅显式 `wait: false` 创建句柄。

句柄只允许主剧情任务修改或等待；它保存会话内交互编号，宿主快照恢复按当前会话重新核对。每次 wait 完成登记此检查点之前的内容来源；追加部分独立参与已读判断。历史沿原省略/替换策略，append 的 history 选择追加显示正文、替代正文或省略该段；原组已省略或清除时不重新建立历史。文本与历史共享不可变正文，只在实际追加时更新投影；不逐帧拼接或保留历次正文版本。

滑条与按钮共享可组合子节点：`slider(..., children: [])` 的子节点按 stack 布局，`decorated: false` 关闭内置轨道、滑块和焦点描边，保留输入、绑定及无障碍行为。无装饰时数值轨道覆盖内容区完整宽度，作者以归一化 value 放置滑块并自行绘制悬停/焦点外观；padding 仍定义内容区。子节点沿共同舞台的所有权、裁剪和快照路径，不引入第二套滑条输入处理。

`game.dialogue().ready` 与 `lines` 中每行的 `ready` 是只读可观察字段，表示正文和包括尾部停顿在内的时序已完成。组字段仅在当前对白有效且所有行均完成时为 true；行在对白结束后保留 true，清页删除对应行。它不包含自动、快进、暂停或窗口焦点策略，作者与 `game.reading()` 组合决定推进光标的显隐和样式。沿已有叙事投影循环更新，不扫描正文、不创建逐帧任务。

`game.dialogue().advance_requested` 与当前活动行的同名字段是只读可观察的逐段手动推进请求。有效手动 Advance 在允许跳过显现或正文已经 ready 时置 true；尚未完成且禁止跳显时保持 false。自动阅读与快进驱动不会设置它。新对白、append 新段和清页重置；快照保留当前值。互动结束后根字段为 false，仍展示的旧行保留最后一次投影值；用 `current_line_ids` 确定描述当前段的行。作者可用它结束自定义文字演出，输入仍由共同对白服务接受和推进。

### 角色对白与公共播放时钟

对白调用 `character(text, history:true, reveal_skippable:true)`、`narrator(text, history:true, reveal_skippable:true)` 和 `say(lines, reveal_skippable:true)` 在提交时固定本组的揭示策略。false 时，正文及尾部停顿完成前的手动推进被消费并返回 `InputOutcome::Pending`，不补字、不结束等待；快进仍按公共时钟揭示，完整画面实际接受后才推进。默认 true 保留点击补全和快进直达；自动阅读、`[next]`、速度设置、历史及已读规则沿既有路径。玩家速度 0 在 false 策略下仍执行显式 speed/pause，未指定固定速度的片段即时显示。策略只作用于本次调用；无需脚本帧任务或临时修改全局阅读状态。`DialogueInteraction::reveal_skippable()` 读取该策略。稳定揭示更新保持零分配。

富文本值的只读 `.plain` 返回正文，`.markup` 返回创建时的完整标记字符串；两者读取共享已有不可变值，不复制正文。作者可组合姓名前缀与历史标记，普通字符串仍须经过 `rich_escape`。此属性沿现有 rich 宿主值和快照保存，不改变 VM 指令或 native 调用 ABI。

富文本采用显式 `rich(markup)` 作者入口，普通字符串保持字面含义。句内 `[b]`、`[i]`、`[u]`、`[s]`、`[size=32]`、`[color=#ffd580]`、`[ruby=ほし]星[/ruby]`、`[em]` 和 `[link=entry]` 使用成对嵌套标记，`[[` 表示字面左方括号。解析产生平台无关的不可变全文、UTF-8 样式范围、注音与链接身份，共用于 UI、对白和历史；Aside 不依赖这些类型。字体布局复用 reel-text 的范围样式和 inline box，注音与着重号参与行高，ruby 基文与注音作为一个换行单位。揭示仍按基文的字素推进，标记和注音不消耗剧情字符数；链接只在已显示的命中区域交付 `on_link`，不自动打开 URL。段落通过 text_align 与 line_height 设置对齐和行高，舞台位置、尺寸和裁剪控制 ADV/NVL 布局。源码与 ASB 共用此接口；段落按可用宽度换行，逐字显现保留布局缓存，链接使用已呈现的输入几何和节点所有权。错误标记保留 rich 调用位置与标记字节偏移；rich_escape 为显式标记插值转义左方括号。

对白呈现模型逐行公开只读 `id/speaker_id/speaker_name/content_id/text/reveal/read`。旁白 speaker_id 为 null；read 保存该行提交前的已读状态，NVL 累积行不随之后的推进改色。顶层 speaker_id/content_id 与姓名、文本一样投影第一行，read 仍表示整组提交前均已读。交互结束或进入选择时保留所显示对白的身份与 read，直到下一组对白替换；active 单独表示是否等待对白推进。`dialogue.clear()` 同时清空 NVL 行和 ADV 的文字、姓名、显示身份及揭示位置，保留历史和当前对白的等待、推进所有权。后续帧与快照恢复保留空显示页；追加到当前发言时重新显示更新后的完整发言，新对白开始新页。作者用 derive 选择颜色，用普通节点与 tween 组织退出动画，动画不会修改阅读进度。身份字符串在行发布时分配，与同批历史共享，不参与逐字显现更新。

`character(name, id: null)` 创建不可变角色；省略 id 使用 name，显式 id 区分同名角色。角色可直接调用并等待一句，`.line(text)` 返回不可变行描述；`say(lines)` 原子提交非空 ADV 行组并共用一次揭示/推进交互。角色名、身份、完整文本按提交时保存进历史；行描述可复用。旁白沿相同会话协议，回调不能抢占主剧情对白。可信宿主可调用协议由独立 Aside 提供，角色不引入语言特殊语法。组解码按工作额度让出，失败不产生部分历史。

`game.configure(text_speed: null)` 配置每秒揭示字素数，默认 30，0 为即时完整显示，省略保留设置。播放器以公共单调逻辑时间播放，挂起、资源等待及正在配对的输入不积累欠账；完成后停止定时唤醒。文本始终按完整内容整形，时间推进只修改揭示范围。窗口只适配时间和唤醒，Rust 嵌入宿主使用同一播放逻辑。源码/ASB 示例见 [author_dialogue.aside](../examples/author_dialogue/src/main.aside)。

模型来源的依赖捕获使用 Aside 的私有有序强边载荷与平衡树索引，去重和捕获期间的写冲突查询不再逐项扫描所有已捕获依赖。索引按堆预算准入、分片回收，捕获嵌套、取消与失败保留同一提交语义；VM 快照 恢复每个值块时重建已映射身份的索引，来源生成、捕获、订阅、列表索引、首窗、定位有效性和远跳分别测量；依赖有效性查询仍与登记依赖图有关，虚拟窗口不意味着数据与来源索引按需加载。

冷查询和订阅提交沿私有 Dependencies 的既有 64 值块读取，每块复制到固定栈空间后按原顺序处理依赖，省去逐值重复容器验证与块查找。逐来源绑定解析、环检查、反向订阅准入/释放、弱边与失败发布语义保留；不增加常驻图副本或跨让出缓存。现有同步步骤仍处理全部实际条目，不能按块数缩小预算或声称恒定帧成本。每来源反向订阅数组的初次分配保持原样，仍单列 CPU 与分配成本。

实现取舍参考 [Leptos reactive_graph 的自动依赖跟踪与异步 effect 合同](https://docs.rs/reactive_graph/0.2.16/reactive_graph/)及其[有序来源/订阅集合](https://github.com/leptos-rs/leptos/blob/main/reactive_graph/src/graph/sets.rs)。后者当前采用 FxHasher 与 IndexSet。Aside 保留自身动态字段、任务续体、GC 强弱边及可恢复捕获，不另套 Rust closure/Owner 调度；捕获查询使用标准库 BTreeSet 索引，插入顺序仍由强边序列保存。此方案接受 O(log n) 查询与树节点空间，避免整表 rehash/连续索引扩容；真实来源有效性检查仍需遍历依赖，后续是否采用有序 hash 集合由剩余负载与峰值测量决定。

作者的数据类使用 Aside 的统一类布局与 GC，Reel 不另建脚本类包装层。实例方法进入普通 VM 调用栈，构造和字段初始化沿用 NoWait 与可让出的执行预算。

`model` 的数据图包含已完成构造的类实例；复制实例和字段值，保留别名与环，共享类定义，不重跑构造。后续赋入实例保留原身份并观察其数据字段。实例仍满足原名义类型，继承与绑定方法作用于对应实例。字段沿用原语言 cell，作为同一属性图的端点；bind/derive 与控件回写保留只读和完整类型约束，失败不改变字段或已连接控件。静态字段不成为实例状态；未完成/失败的实例、可执行或资源字段不作为可复制默认数据。源码定位、GC 与复制取消沿现有路径；稳定字段读写不扫描类成员表或创建载荷。

脚本堆的对象、宿主根和根区间存储按实际需求增长，默认不以固定数量或字节额度限制作品规模；宿主可以显式设置资源配额。GC 与释放队列在对象准入前准备容量，增长保留代际身份与扫描状态；队列、根映射及增长时的新旧存储都进入同一内存计账。执行任务和切片预算独立于这些堆存储额度。

VM 对象的代际身份与实体存储分离：slotmap 保存小型位置记录，32 对象实体页保持紧凑，清扫按对象交换移除并释放空尾页。旧键的代数保留，长期运行不再因对象数量峰值保留同等数量的完整 Object；实体移动对 Value、根和快照透明。释放实际分配后才退还容量计账，GC 不为整理对象申请新存储。

播放器只在 VM 分配压力达到阈值、集合清空请求回收，或执行遇到资源阻塞时启动新一轮 GC；普通脚本切片结束不再单独触发。进行中的 GC 继续按原切片预算推进，新分配和清理请求保留给下一轮。Aside 提供常数成本的压力查询和回收请求，具体计账见语言规范；显式 GC 保持原语义。

`game.start(entry)` 已实现为新会话入口：entry 是当前 ASB 中不捕获局部状态、无必需参数的普通函数。调用位置先验证入口；请求接收后旧脚本不再继续，宿主结束旧任务、对白、活动和音频，重新执行模块初始化，再调用 entry。已读记录与 game.configure 配置保留；脚本全局、历史、自动/快进状态重建。资源源和存档服务由宿主复用，音频工厂独立于存档安装。替换等待上一会话清理和已提交存储操作完成，避免无限堆积退休会话；清理仍按片推进，程序共享已验证字节码，不重新编译。尚未提供跨 ASB 热更新入口解析或持久脚本全局。

宿主 Awaitable 复用 Aside 的 native 等待帧，不另建引擎专用 VM 等待系统。共享活动由 adapter 保存一次终态及登记顺序；每个消费者拥有独立 WaitId，取消消费者只解除其订阅。活动目标/owner 的取消由活动自身账本负责，再向仍有效的消费者交付结果。Await 的对象、返回槽、继续位置和故障源都由既有 VM 根持有。Reel run/tween 与逻辑时钟通过该协议接入。

宿主函数也是可保存、传递、返回和可选调用的普通函数值。编译器保留可信注册身份，VM 在模块常量根区持有函数对象，重复取值不分配、不增加作者全局变量。动态调用直接进入 NativeFrame，与直接调用共享参数、缺省 Cell、效果和等待处理；不插入脚本转发帧。`text` 可作为组件工厂参数，`narrator` 经变量调用仍产生原来的交互等待，完整源码与 ASB 示例见 [function_values.aside](../examples/function_values/src/main.aside)。
会话管理主剧情、UI 回调任务、交互数据和活动所有权。启动与主剧情任务归会话所有，UI 回调归发出事件的节点所有；替换场景结束旧节点及其活动，主剧情任务继续。主剧情同一时刻只有一个交互。game 服务身份稳定，game.dialogue 属于当前会话，作者可用 model 定义设置字段并通过 persistent 服务持久化。发言更新完整文本与显现范围并追加一次历史，再等待推进；文字未显示完时首次推进只完成文字，后续推进才结束等待。choose 将输入普通表复制成引擎拥有的只读可观察选项行，id、key 与 label 均可读，id 是 InteractionId 加 key 的不透明稳定标识，列表用 id 区分连续选择；select 接收带 InteractionId 的 option.id，验证后返回业务 key。入口与剧情 helper 均为普通 fn，剧情元数据按需附加；具体语法见语言与 DSL 文档。
VM 的 run_slice 接收指令、内部工作、分配和计时检查额度，返回 Returned、BudgetYield、HostCall、Waiting、Fault 或 ResourceBlocked。ResourceBlocked 保留帧、根和中间故障阶段；不能把它视为普通可丢弃错误，也不能假定 GC 会释放仍活跃的 catch/展开记录。宿主调用携带任务身份、参数和源码位置，返回 Ready、Pending、CallScript、Continue、Error(NewFault) 或 ForwardFault(FaultId)。Continue 保存 native 算法状态并于同 Task 下一预算片继续，无外部等待。CallScript 保存纯数据 native continuation，退出原生栈后在同一 Task 压入脚本回调帧；回调可按其效果上下文挂起，返回/异常恢复 continuation。Pending 先退出 Rust 调用，再建立显式恢复记录；恢复记录保留操作、可恢复参数、返回目的槽与下一指令位置。禁止把原生调用栈当作可保存续体。同步原生重入 VM 不得跨边界挂起，异步宿主算法使用显式状态机。
SessionEpoch 在新游戏、读档和回滚时更新；任务、等待、节点、回调和活动各用独立代际身份。后台完成结果只有与仍存活的 SessionId/Epoch 及等待匹配时才交付一次。取消、节点销毁和会话替换使旧结果失效。
ActivityHandle 本身实现 Awaitable，支持暂停、继续、停止和到终值完成。stop 保留当前值返回 Stopped；finish 对有限活动执行到终态返回 Completed；无限重复没有完成终点，由 stop 或 owner 结束。终态为 Completed、Stopped、Cancelled、Failed，只提交一次；前两者返回相应结果，后两者在等待位置产生取消或脚本错误。所有者取消任务时直接终止该等待。所有 VM 任务遵循同一可等待对象契约，恢复身份进入快照。主剧情输入权限仍由会话管理，与函数声明形式分离。
演出描述先纯构造，再由 run 在显式所有者下启动；顺序组合不接收已运行句柄。动作激活时读取起值。活动只在动作段激活时取得写入权，新段从当前值接管并停止冲突的旧根活动，暂停活动亦如此。sequence 内重复属性按顺序使用；parallel 各分支的写集合必须互斥。直接给正在由活动写入的属性赋值会停止对应根活动，包含同值赋值，不影响其他属性的活动。显式 stop 使根活动及余下子动作返回 Stopped；任一子动作 Failed 则结束整组并返回错误。节点与效果目标使用有稳定身份的引用，run 校验目标有效；活动保活脚本对象，不延长显式舞台所有权，任一目标销毁使整个根活动 Cancelled，释放全部保留并向仍存活的等待者交付取消错误。所有终态只提交一次，均释放保留。持久活动图包含子活动、进度与等待关系，并提供可序列化恢复描述。描述中不保存闭包，事件回调排入普通任务。接口见 DSL 文档。
已读标记按稳定语句 ID 记录。按住快进由输入保持状态驱动，松开、失焦、进入菜单和读档时清除；自动播放属于会话状态，文字速度和音量属于全局设置。更换 UI 不改变历史。

作者对白与选项使用 `game.dialogue()` / `game.choice()` 稳定服务对象。会话交互变更时发布行组/选项，显现时仅发布当前行的 reveal；已完成行保留到显式清页，历史独立保存。通用 view 容器沿用派生依赖队列与 native continuation 执行组件工厂，成功才替换内容，退休旧子树沿现有节点所有权取消任务。对白与选项界面由作者挂载，使用共同舞台和已呈现输入协议。

作者阅读服务提供稳定的只读可观察 `game.history()` 列表与 `game.reading()` 状态；`game.set_auto(enabled: null, delay: null, condition: null)` 修改自动阅读及全文呈现后的停留秒数（默认 2 秒），`game.pause_reading(paused)` 暂停对白显现与对白/选择输入，普通作者控件继续工作。状态属于会话，UI 更换不重置；新会话默认手动、未暂停。自动阅读仅推进已完整呈现的对白，选项等待玩家操作。失焦、挂起、模态界面、资源未就绪或输入手势期间不累计阅读时间，恢复后重新等待完整停留时间。有限演出与独立音频不因阅读暂停而停止。 `condition` 接受 bool 或 `bind`/`derive` 引用；各参数为 null 时保留当前值及绑定，condition 为 true 时解除旧绑定。只读 `auto_ready` 显示条件的当前值，初值 true。false 只重置自动倒计时，文字显现、手动推进、快进与 `[next]` 保持原路径；恢复 true 后，全文实际呈现完毕再等待完整 delay。条件及绑定随快照恢复。

历史组提供 `group_id/interaction_id/locale/lines/speaker_name/text`，行提供 `id/speaker_id/speaker_name/content_id/text`。行 ID 与对白模型一致，在同一会话中稳定。历史全文在提交时记录一次；清页和界面替换不删除历史。首次访问按已有历史建立投影，之后只追加新增组，显现更新不扫描历史、不复制全文。数据/控制服务不创建 UI，作者用通用组件组合回看和阅读控件，示例见 [author_reading.aside](../examples/author_reading/src/main.aside)。恢复点、语音重播和已读跳过依赖各自后续服务，不在历史记录内提供占位字段。

`game.clear_history(keep_last: int = 0)` 删除较早历史并返回 null；Rust 宿主使用 `Session::clear_history(keep_last: usize)`。0 清空全部，正数按实际记录的历史行保留末尾至多指定数量，`history:false` 行不计数；数量不少于当前行数时不改动，负数或非整数在原调用位置拒绝。边界可以落在一个组内：只删除该组较早的行，保留组 ID、locale、剩余行的 ID、内容、对象身份及顺序，不重新编号；组 speaker_name/text 更新为剩余首行。顶层历史数组、保留组及其 lines 数组保持身份，变更通知沿现有观察协议发布。完整保留组不重建；后续新记录正常追加，组 ID 不复用。当前对白、NVL 显示页、推进权限、已读进度、阅读设置及系统数据不变，不重新记录已裁掉的当前对白行；已保存槽位不改写，加载它仍恢复保存时的历史。裁剪是同步线性工作，不按 VM 预算自动拆分；只移动保留引用，不复制正文，释放前段容器和内容强边并请求增量 GC。作者持有的行、富文本与界面缓存继续有效，应按需移除或刷新；不强制收集整个 VM，也不默认截断历史。验证覆盖跨组/组内保留、稳定对象和通知、继续追加、正文推进、1 万/10 万行裁剪成本、GC 释放及快照恢复。

历史投影复用 `Heap::commit_collection` 受信宿主提交入口：只将同种集合的私有准备载荷交换进原对象，不改变原身份和元数据，完成后只失效通知一次。历史路径限定 Array→Array，原对象和准备对象都必须有强根，准备对象不可泄露给脚本；全部保留引用复制完毕才提交；若准备过程跨越可修改边界，调用方需验证源 content_version，种类检查失败不交换。旧载荷留在准备对象供增量 GC 回收，调用方释放准备根并请求收集；不逐项移动可观察数组，也不增加第二套批量实现。

`game.set_skip(enabled, unread: false, after_choices: false)` 控制快进，`game.reading().skipping/skip_unread/skip_after_choices` 可观察当前状态/策略，`game.dialogue().read` 表示当前整组在本次提交前已读。默认在未读组和选项处停止；unread: true 允许略过未读对白。after_choices: true 在选择处保留模式，仍等待玩家选择，选择后继续按已读策略快进。快进沿原控制流执行，略过文字显现和停留，并让主剧情拥有的运行中有限演出按既有 finish 语义到终值；无限、暂停或节点拥有的活动与独立音频保留原控制。自动阅读与快进互斥。暂停阅读、失焦、挂起或出现模态界面清除快进；键盘/手柄保持通过 input 回调在 pressed 启用、released/cancelled 关闭，不添加固定快捷键。input 发起的快进登记节点身份，退休时清除保持状态；普通按钮与主剧情设置的模式属于会话。

实际接受的手动对白推进（指针、键盘、手柄或无障碍确认）执行通常的显现/推进，并关闭自动阅读与快进；过期或未接受的输入不改变模式，内部计时推进不触发此规则。选择确认继续保留自动阅读策略。

已读只在对白组完成推进后登记，取消和仅显现全文不登记。Session 使用按 content_id 索引的 `ReadProgress`，提交时预留空间，完成时移动已有内容 ID，输入热路径不分配。Rust 宿主通过 `Session::new_with_read_progress` 复用记录，`ReadProgress::ids/from_ids` 对接自己的持久存储。内容 ID 来自当前剧情的逻辑调用路径：模块与函数身份、代码版本和各层调用位置。不同剧情位置经过同一个对白组件仍有独立身份；重复执行同一路径沿用已读状态。组内行追加行索引，译文与调试源码映射不参与身份；同一编译程序的源码执行和发布 ASB 一致。VM 借用现有任务帧提供调用位置，Reel 仅在发布对白时按路径长度计费并生成摘要。历史的 group/line ID 是会话展示身份，不作为跨会话已读键。

`game.clear_read_progress()` 与 Rust `Session::clear_read_progress()` 显式清空内容已读记录，适合作者提供的进度重置操作。当前等待对白组及各行的 `read` 同步变为 false，之后提交的对白重新视为未读；当前对白正常推进后仍会重新登记。仅跳已读的快进立即停止，允许跳过未读的快进和自动阅读保持原策略。已完成的历史与 NVL 行保留提交时的阅读样式。清除释放原记录的字符串和表容量，成本与记录容量成正比；先为当前等待组预留完成所需空间，保持输入提交路径不分配。恢复存档时，等待组的已读标记按宿主传入的当前 `ReadProgress` 重算，同样预留完成空间；旧快照不能重新带回已清除的等待组标记。它不执行磁盘操作、不删除剧情槽或系统数据；外部已读存储由宿主同步。示例见 [author_reading.aside](../examples/author_reading/src/main.aside)。

`narrator(text, history: true)`、角色调用和 `character.line` 接受同一个 history 参数：true 记录显示全文，false 不记录这一行，string/rich 记录指定的替代全文。显示、等待、揭示与已读身份仍依据原对白；历史在提交时冻结内容及语言，随同一执行快照保存。混合行组保留被记录行的原 ID，全部省略时不新增历史组。正常记录复用已发布的不可变文本，替代文本只构造一次；无逐帧历史扫描。

`game.record(text, speaker: null)` 直接记录一条历史；speaker 可为 character。它不显示对白、不占用主剧情等待、不标记已读，普通剧情、任务和控件回调都可调用，纯派生和初始化不能产生记录。作者可在启动背景语音时记录文本，并自行决定是否等待音频。记录使用同一历史组身份、语言快照和存档，不创建第二种日志；成本与新增文本长度成正比。

`tr(source, id: null, params: null, locale: null)` 可为单次求值指定语言，不切换全局语言。查找指定语言、基础语言、原文，参数模板按所选语言格式化；现有语言包与缓存共享，不复制目录。作者用普通布局展示双语，并选择各行的揭示方式。locale 省略时行为不变。

### Owner 撤销与执行器整合边界

宿主协调完整 owner 撤销。先将 owner 标为 Revoked，禁止新增任务、根、等待注册和过期完成交付；Vm::cancel(owner) 只取消匹配任务并使硬取消票据失效，后续按预算清理任务。Idle 仅表示没有匹配 Task，不能解释为宿主根或适配器已释放。宿主生命周期记录在 owner 创建时预留有界退休容量，撤销时保存 cursor=0 与 end=heap.root_slots()，每个主线程预算 turn 推进 release_owner_step，传入 slots=min(剩余 work, end-cursor)，该 API 不直接接收 end，以 cursor>=end 判定扫描完成；按游标实际增量计费，空洞同样消耗工作。没有 Task 也必须推进根扫描。该扫描仅释放单根，宿主持有的非任务 RootRange 需独立有界释放游标，不能遗漏。任务清理、根扫描和外部资源释放全部完成才进入 Retired，随后对象由预算 GC 回收；宿主根记录同步失效，避免重复释放或继续使用旧 FaultRoot。

源码贯通检查执行普通调用、缺省与闭包、异常/finally、索引/成员、可选链、spread/rest、模块链接和类，覆盖微小预算与交错 GC。narrator/choose 的真实等待、原 key 根交付、取消及播放器终态沿同一执行器处理；同包快照检查等待、任务和宿主状态恢复。property-bag、delegate/hooks、剩余结构模式与原生 async/generator 仍待实现；平台设备和性能采用各自的实际负载验收。

### Rust 会话宿主实现状态

reel-engine 管理 Session/Epoch/owner、唯一主交互、完整文本与字素揭示、稳定选项、过期和重复输入过滤、历史及终态一次交付。Session::new(SessionId, Epoch) 不接受人为行数、选项数、历史组数、owner 数量或文本长度上限。所有会话存储按实际内容使用可失败的内存申请，历史完整保留，不在容量阈值处截断或逐出。DialogueLineId 的进程内索引使用 usize；owner 的完整身份仍单调生成，只有完成退休后才可复用其存储槽。提交前完成文本复制、字素索引、历史/owner 容量申请及选择 key 去重，失败不发布半个交互或消耗身份；选择去重使用借用原 key 的哈希集合。已接受交互的揭示、输入和终态交付仍不分配。会话投影按实际内容高度布局并滚动；滚动后须等待新几何被呈现接受，旧 Down/Up 不能选择新位置。

owner 退休记录在创建时预留，单根游标、任务和外部资源分别设完成门；游标按实际推进含空洞计费。真实适配器仍需调用 Heap/VM 清理，非任务 RootRange 独立释放；纯 Rust 零义务取消不能代替 VM owner teardown。只有被呈现接受的投影才能路由点击，Down/Up 绑定同一投影；领取 Resume 后仍需在实际交付时复核存活 owner 和真实等待身份。

宿主值读取使用公开 sequence_len/get 与 table_own_string_lookup/step。own-string 游标按条目、字节和完成工作计费，区分缺失与 null，不调用 delegate/hook；调用方用真实 native ticket 或显式 root 保活 table，游标本身不持根，跨轮检查存活与完整内容版本。返回 Value 跨借用期保存仍需显式 root。原始 alloc_record(Table) 存储 schema 不是语言 table，适配器不得猜布局。线性查找复杂度和同步常量加载限制见 VM README；owner/RootRange/外部资源的有界退休责任不变。

### Aside 会话适配

共享的宿主描述按各 native 的 ABI 版本生成编译器 prelude/linkage 与运行时签名验证，包内声明不能授权自身能力。真实模块初始化器先于模块级普通入口运行；私有入口按精确 HIR SymbolId 映射输出函数，不通过尚未初始化的 global 猜测。narrator 与 choose 属于主剧情 TaskId；节点回调和 spawn 使用同一 VM 的独立任务、等待与所有权。

交互先暂存并验证配置的会话资源额度；VM 接受 Pending 后才提交可见 Session 状态。实际恢复时复核 interaction、owner、task 和完整 wait 身份；选择的原始 key 保持显式根到交付。owner 撤销分别推进冻结高水位的单根扫描与任务清理，空洞也计费；无任务不等于无根。GC 经过 VM provider。

host_work 仅计选择查找工作，文本复制及发布同步，读取按块推进且不另设总文字长度门。作者文本仍须符合 VM 的实际堆内存预算；会话复制通过可失败的内存申请处理真实分配失败。内容身份由模块路径、版本、源区间或函数/PC 后备组成，不宣称是本地化或跨保存稳定身份。

### 集合展开的执行与成本

Aside 的数组和表展开均由 VM 续体分片执行，源内容版本在捕获期间保持一致。
编译器在每个展开位置完成浅捕获，再执行后面的字段或元素表达式；内部引用共享。
表的显式键和值全部求值后再构造，错误和取消沿既有帧清理释放私有快照及结果。
Aside 字节码 的 TableSources 保留 Pair/Spread 次序，固定表描述符保持原样；空表复制不逐项查重，非空表合并仅扫描目标原有条目。
数组与空表复制的工作量和载荷分配由 aside-vm 的 collection_copy_cost 检查核对。
源码、Aside 字节码、舞台与真实对白等待的贯通入口是 [collections.aside](../examples/collections/src/main.aside)。

## 八 资源与音频

静态音频解码按实际分配阶段预留峰值：已有 PCM 容量足够时只计现有缓冲与当前包转换暂存；仅扩容阶段计新旧缓冲重叠，最终 Vec→Arc 转换计输入容量和输出长度。不得在每包重复计一份未发生的重分配。PCM 准入诊断包含所需字节。

共享资源读取区分单资源过大与瞬时容量争用：文件超过宿主 input_bytes 容量时报告资源路径和所需/配置字节；并发读取暂时占满输入容量时保留尚未提交的读取，随既有资源维护继续准入，不结算为加载失败。取消可直接撤销未提交读取，无额外文件缓存或同步等待。
大图按可见区域加载的实施合同见 [prepared images v1](prepared-images.md)：作者提供原始 PNG/JPEG，通过 `reel prepare-image` 生成 `.reel-image` 与相邻瓦片目录，仍由普通 image 节点使用。格式、元数据布局、共享几何、租约、预算、取消与呈现等待由该合同定义。

资源以稳定 AssetKey 和内容版本管理读取、解码、上传与缓存。取消预取、场景退出与过期结果都释放所属额度。图片、字形、视频队列、GPU 纹理和临时目标分别记账；缓存只保留可逐出的额外引用。直接资源引用与作者预加载共同提供预取提示，动态分支确定后调整集合。

wgpu 图片缓存按完整 ResourceHandle 复用已提交的纹理；准备失败或未提交的上传不发布到缓存。RGBA8 图像按连续行转换为线性预乘 RGBA16F，直接写入映射的 staging buffer，不分配整图 f32 或额外打包副本。准入要求完整驻留纹理与最小一行对齐上传空间可共存；每批使用剩余额度上传尽可能多的行，提交后推进游标，各行只转换一次，完整提交后才发布纹理。更换候选或资源退休撤销不再需要的部分上传，在途批次继续保留纹理和像素至 GPU 完成；设备代数销毁全部部分进度。完整驻留已超限时报告 image residency 永久拒绝，不重复延后。纹理驻留票据随共享图片保留，staging 票据随原提交完成释放；后续帧只登记其使用的驻留额度。额度不足时逐出没有准备中或在途使用者的缓存，资源退休清除缓存引用；在途资源等 GPU 完成再释放。设备代数不共享缓存，其他 HAL 后端保持各自资源策略。
物理像素边界对齐的轴向矩形裁剪使用相交的 GPU scissor，不创建离屏目标；旋转、分数像素边界和任意形状继续走覆盖率合成，保留抗锯齿。目标预算规划与重放使用相同判定，透明组和遮罩继续保持隔离合成语义。
默认音频用 Kira 适配音乐、语音、音效与 UI 分组；各目标输出桥接单独验证。脚本发送控制，实时供给不得执行文件读取和 VM 工作。控制队列有界，连续音量更新可以合并；播放补给拥有独立工作容量与队列额度，图像解码不得耗尽它。短音效预解码，长音轨采用流式策略。[9]
音频自动准备合同：普通 `audio.play` 与 `audio.queue` 由宿主依据解码元数据选择帧存储，作者调用保持不变。`MediaOptions.static_pcm_threshold_bytes` 默认8MiB，以归一化双声道f32的总帧数乘8判断；已知结果不超过阈值时静态预解码，超过阈值或长度未知时使用有界流式PCM窗。`MediaOptions.stream_window_frames` 默认16384帧；阈值和窗容量属于宿主性能策略，不根据音频分组改变播放语义。静态与流式来源共用同一个Kira transport、插值、增益、声像及连续条目队列，混合来源或采样率不更换活动身份；补给不足时输出静音且内容位置冻结，恢复不积累欠账。
流式读取沿既有AssetStream与资源版本，解码复用Symphonia工作线程；固定容量PCM窗、当前包转换暂存及读取缓冲均按实际持有容量计账，释放旧资源后才归还额度。实时消费只读取已准备的样本和控制槽，无分配、文件IO、锁、主线程等待或线程join。定位到未缓存位置与循环回绕发出新预取请求，按源帧身份验证结果，旧请求不能冒充新位置。所有者退休同时撤销IO与解码；后台完成后才能发布资源物理退休证据。长曲的PCM窗驻留量与曲长无关。当前预算涵盖Reel持有的窗、读取缓冲、当前压缩包、codec输出样本容量及归一化转换暂存；codec可能保留的样本按高水位持续计账。Symphonia容器/codec其余工作空间与内部先分配后返回的路径尚未接入预算钩子，因此该账本不代表解码总内存硬上限，任意输入的完整解码预算封闭仍待实现。
未知长度在EOF时发布真实帧数：非循环按实际末尾完成；整曲循环据此回到loop_start；显式end或loop_end超过真实末尾时通过原活动报告失败。seek在元数据未知时先接受目标，EOF确认目标超末尾后按真实末尾完成；audio.at继续观察实际内容位置及终态。快照保存资源版本、绝对内容位置、区间、暂停/增益与连续队列逻辑，恢复重新准备帧存储，不保存PCM或decoder指针；选择静态或流式不会改变同一快照的业务语义。验证须覆盖完整长曲、低于全曲PCM的预算、seek/loop、跨采样率混合队列、暂停/取消/恢复和实时分配计数。
预分配回调路径、锁竞争、声道与采样率协商必须实测。Windows 8 声道设备配置列为重点用例。控制队列为停止、设备关闭与音轨释放预留独立容量；播放请求进入普通有界队列，满载时返回带原请求的 Busy。音量按音轨合并为最新值。停止请求按音轨幂等合并，关闭状态优先于新播放，保证回调线程始终有界且主线程获得明确结果。具体容量由目标配置给出，满载场景进入契约验证。
视频通过可替换的 reel-media 解码适配器连接 AssetIo。首个软件适配器选用固定版本 OpenH264 0.9.9、mp4 0.14.0、h264-reader 0.9.0 与 Symphonia 0.6.1，处理 MP4 中 H.264 Baseline 与可选单声道/双声道 AAC；不引入生产 FFmpeg 依赖。Baseline 无 B 帧，按容器 PTS 展示；当前适配器拒绝需要重排序的 profile，媒体架构仍保留独立 PTS/DTS。平台硬件解码器可沿同一输出契约扩展。音轨通过有界 PCM 队列进入现有混音器，视频使用有界帧队列与可复用纹理；不得预解码整片或逐帧创建 PNG 资源。音频可用时以实际消费的有效 PCM 内容时间为主，欠载不推进内容时钟；无音轨时使用会话时钟。暂停、seek、取消、EOF 排空与所有权退休共享媒体身份和唯一终态；seek 更换代数，旧帧只释放。解码线程可等待独立 IO 队列及背压，主线程和音频供给不得等待或执行 IO。当前 CPAL 无可信硬件呈现时钟，内容同步不能称为设备采样精确同步。透明遮罩、平台硬解和其他编解码组合分别实现和验收。

首个适配器的可执行 Rust 接口是 `reel-media::video::VideoPlayback`，由 `video` feature 启用。当前接受非分片 MP4、渐进式 8-bit YUV420 H.264 Baseline 与可选 mono/stereo AAC-LC；容器的连续 edit list 用于去掉 AAC encoder delay/padding，seek 按 sync sample 回退并解码参考帧。SPS/VUI 提供矩阵、范围及传递函数；未指定时采用 limited range、BT.709 transfer，720 行及以上用 BT.709 matrix，其余用 BT.601。明确的其他矩阵/传递函数报告不支持，不默默改色。

`VideoOptions` 的默认队列是 3 个已解码画面和 8192 个音频采样帧，可由宿主配置；另有一个解码中的输出。画面 lease 与 PCM ring 按实际容量持有 `BudgetTicket`，消费者保留的 lease 继续占额度。MP4 索引随 sample table 大小增长，原生解码器的参考帧/内部工作区由解码器管理；队列预算不等同于进程总内存。PCM 回调只消费预分配环并发布内容时钟，不分配、读取文件、取得互斥锁或唤醒任意宿主回调。音轨通过现有 Music 总线和后台回收服务播放；音轨早于视频结束时由会话时间推进尾段。单视频 `set_volume` 与 Music 总线音量相乘，不修改内容时钟。暂停保持位置，stop/cancel 立即撤销供给并唤醒 worker；宿主等待 `worker_finished` 完成后台退休。恢复/seek 新建播放代数，撤销旧 reader；`start/end` 表示精确的源内容区间，end 是开端点。

`Assets::stream` 将 worker 的 Read/Seek 请求转交共享 AssetIo，主控仍调用 Assets::maintain；没有第二条文件读取路径。DirectoryIo 首次 describe 在 IO worker 用 64 KiB scratch 流式计算内容 digest，缓存打开的文件与 digest，后续 range read 不重复扫描；散文件首开成本与文件大小成正比。具有已知内容 digest 的资源包直接提供该版本。取消/服务关闭唤醒挂起的 reader。当前 Rust 解码/音轨契约见 [video_playback.rs](../crates/reel-media/tests/video_playback.rs)；舞台视频节点和 Aside 接线在独立呈现变更中完成，本段不宣称图形设备已经验收。

### 作者音频活动

统一初始暂停：`audio.play(..., paused:false)`、`video.play(..., paused:false)`（含节点 `.play`）、`clock(..., paused:false)`、`run(..., paused:false)` 和 `scope.run(..., paused:false)` 在创建边界接受 bool 初始状态。媒体把暂停状态随首次后台请求交付；读取、解码和首帧准备继续，内容位置保持 start，resume 允许内容推进。seek 更新位置后仍保持暂停，resume 可在解码完成前调用；音频首批仍在创建任务显式等待或返回时提交。暂停 run 完成描述图的预算构造与验证，首次 resume 才激活、读取起值并取得属性写入权；空组合和零时长动作保留未完成状态至 resume，finish 主动完成有限动作，stop 保留当前属性。clock 在首次 resume 前保持 ticks=0、elapsed=0.0。作用域和节点退休沿现有取消路径。

Rust MediaBackend 的 play_audio/play_video 接受 paused bool，视频 worker 在创建 transport 时设置暂停；复用已有 AudioPlayback/VideoPlaybackState、活动 Paused 与图未激活状态。现有存档同时保留未提交音频批次和暂停状态，恢复后继续同一首批及活动。稳态继续现有暂停索引和媒体维护成本，构造少一次 pause 分派。作者可先创建暂停媒体或时钟、绑定画面，等待 stage.presented 后调用 resume。

Windows CPAL/WASAPI 输出协商客户端 Mono/Stereo，使用共享音频引擎的声道矩阵转换；设备默认多声道格式不再被单/双声道筛选排除。其他后端仅公布其已知有序布局，不根据未知原生声道数量猜测扬声器位置。

`audio.play(source, looping:false, volume:1, owner:null, group:"effects", pan:0, fade_in:0, start:0, end:null, loop_start:0, loop_end:null)` 立即返回可等待活动，后台完成读取与预解码后开始播放。`volume` 是非负线性增益，0 静音；`audio.volume(activity, value, duration:0)` 更新增益。`pause`、`resume`、`stop` 和 `await` 与演出活动共用作者入口。await 返回 Completed 或 Stopped；读取、解码、输出失败抛出错误，重复等待保留相同终态。Completed 表示混音器消费完内容，不承诺声卡已经播放完。

`audio.queue(activity, source, start:0, end:null, looping:false, loop_start:0, loop_end:null)` 向同一音频活动追加片段，成功提交返回 null。参数沿 play 的位置/循环边界验证一次，终态活动拒绝追加；读取、解码或输出失败通过原活动唯一终态传播。owner、group、volume、pan、暂停和停止作用于整条队列，seek 作用于当前片段；position 是当前片段素材的内容秒数。当前片段循环且已有后继时，在本轮 loop_end 退出；没有后继则继续循环。已准备好的片段由混音回调在内容边界接续，不经过主线程重新 play。后继未准备好时输出静音、保持内容时钟和未完成活动；准备好再继续，无缝保证仅覆盖已准备好的片段。

新音频在创建任务的同步执行段结束前保持静音，后台读取和解码可以提前进行。任务开始显式 await、普通调用真正 Waiting 或任务 Returned 时通过 MediaBackend::commit_audio(id) 冻结当时已知片段作为首批，等待这些片段完成准备；BudgetYield 和普通 HostCall 不封口，后续追加不延长已封口首批。任务故障或取消撤销尚未提交的声音，不短暂起播。未提交声音的 TaskId 随会话快照重映射，恢复后继续等待原任务边界；已提交声音在恢复其完整队列后重新提交。队列请求、待解码、PCM 和退休都有界：沿宿主现有 mixer.max_sounds 限制未退休片段与队列描述符，PCM 与解码继续使用 pcm_bytes、queue_bytes 及其 BudgetTicket；原生队列固定存储按 mixer 的实际容量计入 AudioActivityService::slot_bytes(slots, mixer)，不假设活动槽数等于混音容量。没有作者容量参数。回调只消费预分配队列、切换播放游标和移交退休载荷，不分配、执行 IO、等待或析构 PCM。当前片段身份、实际位置、循环退出状态、待播片段和暂停状态进入 AudioPlayback，会话快照使用当前 v1 格式。

音频终点（已实现）：start、非 null 的 end 与循环边界使用有限非负秒，显式 end 必须大于初始 start；null end 使用资源结尾。end 是排他内容终点，后台按现有 round 规则量化到源采样帧并检查位于资源内且至少一帧，通过 StaticSoundData 的 [0,end) 视图共享原 PCM。[0,end) 视图至少一帧不要求量化后的 [start,end) 非空：两端落入同一源帧时无声自然完成；恢复位置恰好为 end 沿同一规则完成。start 仍是初始绝对位置，不是 seek 下界；position/seek 始终使用当前素材的绝对内容秒数。looping:true 时循环区间为 [loop_start, loop_end)，null loop_end 使用 end；循环区间须位于该视图内、量化后至少一帧。start 小于 loop_start 时先播前奏，超过循环终点时按区间取模。looping:false 仅接受默认循环边界，seek 达到 end 后自然完成；加载中可先定位，解码后按最新起点钳取。混音传输在终点完成或接续队列，沿同一重采样历史和退休路径，不用主线程定时 stop，也不复制 PCM。每个队列条目保存自己的 end，恢复保留当前条目实际位置与待播边界。检查源帧排他边界、44.1/48k 重采样、已准备队列连续样本、暂停/seek/循环退出、终态和源码/ASB存恢复；实际声卡呈现另验。复跑 `cargo test --locked -p reel-media --test audio_contracts playback_end`、`cargo test --locked -p reel-player --lib media::tests`、`cargo test --locked -p reel-engine --lib aside::tests::snapshots::audio`，以及 `cargo run --locked -p reel-dev -- audio-probe`。

`audio.position(activity)` 返回最后观察到的内容位置秒数，加载中为请求起点，暂停或输出挂起时保持，循环时回到区间起点；终态保留最后位置而不保留 PCM。在 derive 内读取会订阅同一内部只读位置，普通读取返回最后发布值；无 IO、解码或完整快照复制。切换同进度音轨可用 `audio.play(next, start: audio.position(current), group:"music", looping:true, fade_in:2)` 与 `audio.stop(current, fade:2)` 组合，淡入从新音轨实际起播时开始。该组合不承诺两个后台加载音轨的采样级同步。

`audio.at(activity, seconds)` 和 `video.at(activity, seconds)` 返回可重复等待的位置条件。seconds 必须有限非负；同一活动发布的位置达到或超过目标时返回 true，目标前正常 Completed/Stopped 返回 false。终态先按最终位置判断，取消和失败沿原错误传播。已结算结果保留，后续 seek 回退也保持；取消等待者只撤销该等待。loop/queue 使用当前片段的内容位置，回绕后继续观察，取样间隔中经过的区间由下一次观察决定。seek 立即发布宿主报告的位置并结算满足的条件；此显式操作按全部待决位置条件线性扫描，维护采样按 64 路轮转。位置条件归创建时的默认 scope/回调节点，所属范围退休会取消条件并保留声音自己的 owner。音频和视频共享位置、条件和快照路径。

媒体后台维护收集最新位置；SessionVm 在同一响应式采样批次内发布媒体位置与普通活动时间。尚有派生计算时保留最新后端样本，终态、取消和视频帧继续维护。位置采样沿 64 路轮转；条件结算使用复用队列，创建和恢复的容量通过既有 HeapReservation 计账，关闭后释放容量并退款；批次中的条件检查完成后再发布下一批位置。首加载、暂停和输出挂起沿后端内容位置保持。软件观察位置表示内容消费进度，设备实际呈现时间仍取决于输出端能力。SessionSnapshot 保存内部位置、未决条件和已结算结果，共享媒体身份随 VM 图恢复。position ABI 是读取已发布 VM 状态的纯 native，at ABI 创建可等待对象； 定向行为命令：`cargo test --locked -p reel-engine --lib media_positions`、`cargo test --locked -p reel-cli --test project_scenarios project_scenario_media_position_cue`。稳态采样及实际派生读取的成本对照：`cargo test --locked -p reel-engine --test media_position_allocations -- --nocapture`，分别记录采样发布与观察者执行，并与相同数量的 clock 订阅比较。

`audio.seek(activity,seconds)` 提交绝对内容位置并返回 null，复用原活动和播放终态。加载/解码中更新起点，解码结果按最新请求配置，不重新读取或复制 PCM；播放中使用合并命令槽，源帧量化和循环规则与 start 一致。暂停或输出挂起仍可定位，非循环超尾夹到末尾，已终态不重启。控制端在 seek 命令被消费前报告最新目标，混音器先清空重采样历史、发布新位置再确认该命令序号，旧确认不得覆盖后续目标；紧接保存使用此逻辑位置。音频供给无新增分配、IO 或等待。

AudioRequest 保存可移植起点、排他终点与循环区间，AudioPlayback 捕获当前内容位置；所有分组均按当前条目实际位置、播放边界、循环区间与待播队列恢复；分组仅控制混音归属，已经结束的音效不重播。音频活动终态位置进入 VM 宿主载荷。未完成淡变沿原有目标值恢复，不引入设备句柄或音频时钟轮询任务。

缺省 owner 是当前回调节点，主剧情中是会话；节点退休取消声音与尚未结束的加载。取消一个等待者不取消共享播放。会话关闭先撤销等待，再停止输出、取消加载及活动，持续回收直到实际 PCM 和后台请求释放。挂起保留 mixer，恢复复用；输出失败只结算一次，不自动伪装恢复成功。

核心通过 MediaBackend 接口接入媒体，不持有平台句柄。播放器将共享 AssetIo 读取与独立解码容量、AudioActivityService 及 AudioOutput 组合。预解码 PCM 按实际容量预留与计账，不设固定音长上限；每包解码检查取消与预算。长轨流式与设备重建继续遵循上文完整设计。音频控制维护使用 10 ms 后备调度，仅有未完成播放/回收时启用，不请求布局或绘制。

### 混音与音频过渡

AudioGroup 位于 reel-core 的平台无关契约；media 与 engine 使用同一枚举。播放器将 music/voice/effects/ui 路由到四个 Kira 子轨道，主轨道负责 master 增益。每声增益、分组增益与主增益相乘，组设置影响已有与后续播放。子轨道只预留声音槽和内部缓冲，不复制 PCM；每组槽容量为 max_sounds，总播放准入仍共享 max_sounds。四组控制句柄随 manager 一起移交原后台退休服务，按轮转游标和统一配额取走已退休声音。

作者 play 接收 group、pan、fade_in；volume/group_volume/pan 的 duration 和 audio.stop 的 fade 使用非负秒。Kira 在音频消费时钟推进过渡，命令通过现有合并槽传递，无逐帧脚本任务。声音暂停保留播放位置，音量/声像包络继续推进，输出挂起冻结全部包络；淡出停止期间再暂停立即停止；淡出停止只在混音器停止后结算 Stopped，加载中或已暂停则立即停止，owner 取消及会话关闭不等待淡出。新音量/声像命令从当前值覆盖前一个过渡；加载中变更只更新初始值。音量按分贝插值，静音端点 -60 dB，声像线性插值。实时回调不执行分配、析构 PCM、文件 IO 或控制线程等待。

### Rust 音频活动层

reel-media 的 AudioActivityService 在 Kira 之上保存有界活动账本。请求拥有固定 ResourceVersion、注册 AudioOwner（包含完整 Scope）、MediaEpoch、分组元数据及 DecodedSound；原生句柄不跨模块暴露。接纳前同时保留播放槽和终态槽，拒绝返还原请求，终态保留槽直到排出。MediaId 在服务实例内不重复；替换实例时由宿主提供新的 MediaEpoch。分组映射到宿主混音总线，循环取自输入声音配置。

DecodedSound 不设置独立 PCM 帧数或时长上限；调用方的真实 BudgetTicket 覆盖输入 capacity 与转换存储，并随 Sound 整个生命周期保留至退休。回收量不从固定 64 MiB 假定推导 PCM 上界。

Completed 表示通过 Playback 观察到内容消费结束，不是设备实际呈现，也不表示 PCM 已释放。Stopped 表示停止命令的逻辑结算；供给停止仍由 AudioOutput 的 fence/stop 协议保证，物理 PCM 回收仍经后台服务和预算票据。播放器将该终态交付 SessionVm 的媒体等待；作者 Completed 表示内容消费结束，声卡实际呈现沿前文设备时钟合同。

撤销 owner 时，宿主先撤销业务等待并禁止该完整 Scope 新增播放，再用有配额的 cancel_owner 游标完成清理。维护、取消和排出按实际槽位（含空槽）计费。接收队列满时保留可交付记录，但继续计费推进游标，使后续取消/失效记录仍可释放。完成一次扫描后不因消费者背压反复自唤醒；消费者释放容量后重新排出。排出以接纳时的 (MediaId, MediaEpoch, Scope) 校验精确业务等待，过滤已失效等待和 Cancelled；输出故障后推进的当前代数不抹去已接纳活动的身份。已结算但尚未交付的 Completed/Stopped/Failed 保留原身份和结果；若延后实际交付，仍需再次校验等待身份。已结算终态不被后来的普通取消重写。输出故障由 fail_output 推进一次后续接纳代数，并通过分片维护将剩余活动按原接纳身份结算失败；宿主负责停止供给。它不代表保位置设备恢复。

output、活动账本和 AudioReclaimService 共用 WakeSignal，宿主先 acknowledge，再排出全部来源，休眠前复核 pending。未扫完的有限工作立即续轮；仍有未结算活动时，poll_after 建议 10 ms 控制侧复查，以补足当前 Playback 缺少逐音轨完成订阅的通知空隙。没有未结算活动时不设置这个周期计时；音频检查不请求布局或呈现。10 ms 是后备调度间隔，不保证过载下完成延迟。

AudioActivityService、MediaPlayer 和 SessionVm 共同实现 seek、pause/resume、分组混音、队列、作者等待及同包持久恢复。输出错误结算活动失败；设备重建、实体设备呈现同步及控制耗时目标仍需对应设备验收。

### 音频 owner 物理退休凭据

AudioActivityRequest 使用服务注册的 AudioOwner 代替裸 Scope；凭据保留完整 Scope 与服务身份，外服务或已关闭 owner 不得准入。retire_owner 先关闭准入，再按持久游标分片取消；业务等待必须先撤销。游标完成仅代表取消扫描完成，不代表内存或描述符释放。

每个已接纳活动分别计资源与描述符义务。全部 PCM 别名释放、最后共享 Residency 的 BudgetTicket 归还之后，才发布资源完成标记；Entry 的 Playback 和资源 key 析构后才发布描述符完成。AudioOwner::is_retired 还要求准入已经关闭。逻辑终态、排空队列、全局共享预算余额均不能替代该精确证据。服务析构关闭 owner，但已取走 fill 或挂起输出仍可能延长资源寿命；PCM 继续经原后台回收路径释放。

owner 注册与活动凭据各有 max_requests 容量上限，保留已完成凭据仍占槽；克隆共享同一登记。相同 Scope 的现存关闭登记不会重开，所有引用消亡后的重新注册是新身份，Session 宿主不能把已退休 Scope 重用为原 owner。播放器媒体后端使用 AudioOwner::is_retired 作为物理退休条件；作者等待通过同一媒体后端连接会话。实体设备与回收时限按目标分别验收。

## 九 存档与回滚

共同节点的 `capture_included` 默认为 true。false 排除该子树的捕获输出，录制时冻结并归并平衡的命令区间；普通显示和输入沿原舞台行为。捕获读取最近已呈现帧，保留转场的旧、新画面各自携带排除区间，显式目标及祖先的 false 同时排除整段转场。绘制数据共享，捕获重放跳过区间，并据此计算图片上传与离屏目标需求。



当前自有协议统一使用 v1：Aside 字节码、VM 快照、SessionSnapshot、宿主 native ABI 与 ASB 1.0。新增能力继续维护 v1 声明，版本升级由明确的 2.0 发布决定；加载按实际声明核对格式、签名和代码身份。

存档路径属于宿主配置。`reel run` / `reel dev` 的 `--save-directory <directory>` 直接设置 `StoryWindowOptions.save_directory`；相对路径以启动时工作目录解析。便携启动器可传入游戏目录内的 `saves`，安装目录只读的平台由宿主选择系统可写目录。未指定时继续使用平台用户数据目录；目录配置不进入脚本或 ASB。

捕获期间未按键的鼠标移动合并为最新位置，捕获结束后按已接受画面重放悬停；不因轻微移动取消保存，也不积累移动事件队列。点击、拖动、键盘或焦点变化仍按会话修改入口处理，并清除此前排队的悬停。

VM 快照底层已实现：`Vm::snapshot_begin` / `Vm::restore_begin` 返回按 `SnapshotBudget` 推进的捕获器与隔离恢复器；完成的 `VmSnapshot` 可送到工作线程编码。当前 VM 格式版本为 1，使用独立的 `ASIDSN01` 标记与 Serde/Postcard 记录，代码身份在验证编译包时计算一次，恢复要求精确匹配。`SnapshotRegistry` 按名称和版本重接宿主属性验证器，`SnapshotMap` 重接宿主保存的对象、根与等待身份。单步处理一个元数据项、64 个值或 1024 字节载荷；分配器增长不构成硬实时保证。对象图、执行帧、异常、取消和中途指令状态的恢复验证见 `cargo test -p aside-compiler --test host_awaitables --test classes`。`SessionVm::snapshot_begin` / `restore_begin` 在同一快照中接入会话、舞台、活动、任务结果、粒子与随机状态；舞台从 VM 属性重建，悬停、按住与焦点清空。历史正文与字素边界在捕获时共享不可变存储，编码在工作线程进行。音频后端以 `AudioPlayback` 提供内容位置和逻辑播放参数，所有音频按当前条目实际位置与待播队列恢复，已结束活动保持终态；候选音频先暂停输出。翻译目录保存资源路径并通过宿主加载器重建。Rust 会话恢复检查见 `cargo test -p reel-engine --lib snapshots`；槽位接口为 `saves.save(slot, title: null)`、`load(slot)`、`delete(slot)` 和 `list()`；请求立即返回可等待的只读对象，`status` 为 pending/succeeded/failed，`error` 在失败时保存错误。list 的等待结果是已占用槽位摘要数组，字段为 id、title、saved_at（UTC Unix 秒）。页码、快速槽和空槽由游戏组合，槽位名不作为文件路径。桌面存储使用独立用户数据目录，Rust 宿主可指定目录、快照预算和 SaveStorage 实现。

存档在所有可修改持久状态的任务停在合法指令或等待边界，且舞台、绑定和资源切换已提交后捕获。保存、读档提交与热重载提交互斥。保存接口立即返回请求，进度由服务模型提供；发起保存的任务返回或停在普通可恢复指令/等待边界后即可捕获，因而允许等待自己的 SaveRequest。当前存档副本内将该请求及其 Future 记为 Succeeded；活会话保持实际写入状态，只有可靠提交完成才成功。读取这份已提交存档时，恢复请求从已保存的后继指令继续，等待者在下一调度步按登记顺序收到成功，保存调用执行一次。此规范化只作用于被持久提交的该存档副本，既不修改活会话，也不适用于内存回滚快照。已结算的进程请求直接恢复保存的终态与结果。其他尚未结算的进程写请求只有匹配精确进程和请求身份及完成记录才重接，否则结算 OperationInterrupted 并保留可处理错误，避免重新写入外部目标。
快照包含引用模块版本、全部存活任务及其调用帧与恢复位置、返回槽、异常状态、寄存器与中间值、闭包和共享 cell、全局对象图、待交付结果、当前交互、组件数据、舞台逻辑状态、时钟、随机数及媒体描述。generator、thread 与其他可等待对象按语言定义保存。对象 ID 保留共享、环、弱引用和类关系；脚本回调及捕获进入同一对象图。
设备句柄通过资源描述重建。每种可达宿主值必须有版本化恢复描述；缺失时报告对象路径，保存失败且保留原槽位。系统服务恢复为同一服务身份。读取在隔离候选会话中先创建身份、再连接引用与订阅，成功后一次切换；失败保留当前会话。
捕获固定会话时钟和媒体逻辑状态，按对象与边游标分片复制到独立快照，窗口保留上一个已接受画面。播放器使用持有会话的捕获器，业务写入在此期间暂停；若实际输入或窗口状态变化需要修改会话，取消当前捕获、以 CaptureBusy 结算保存请求，再处理输入，原槽位保持不变。捕获完成后业务恢复，后台结果继续交付。后台以 Postcard 流式编码并压缩到临时文件，不建立另一份完整编码缓冲。
资源读取按描述重发，动画按逻辑时间继续，音频按当前条目资源、实际位置与队列恢复，已结束活动保持终态。外部写入使用结果记录与回滚边界，不能随读档盲目重放。
槽位文件保存摘要和完整快照。当前目录适配器先同步候选文件，再原子替换槽位；成功承诺完整文件的原子可见性，不承诺所有平台断电后的目录持久性。写入、读取与删除共用有序后台队列，发布前失败保留旧文件。作者可用 stage.capture 获取最后呈现帧，并随 saves.save 的 thumbnail 参数写入同一槽文件。捕获后的写盘由进程级保存服务持有，切场景不取消它。平台同步与替换行为必须实际验证。播放器一次处理一个存储请求，快照捕获/解码与存储队列使用宿主指定的字节预算；请求根和队列计入 VM 配额。旧会话异步结果只按会话及请求身份结算。读档候选完成派生、翻译和舞台资源准备后替换，旧会话执行取消清理，已接受写入在关闭时排空。
全局设置、已读和解锁记录独立于剧情快照。恢复设置绑定时以当前全局设置为准。回滚先使用完整交互快照和共享不可变资源，规划中的回滚历史预算起点为 32 MiB，超限移除最旧快照；活动 cell、属性和等待状态必须独立复制。增量记录等真实成本明确后再做。
每个闭包和挂起调用绑定确定代码版本；旧模块保留自己的全局环境与导入绑定，最后引用退出才释放。发行存档由匹配游戏包提供代码；开发存档嵌入无法从匹配固定包取得的全部所需模块，包括活动版本。缺失版本进入明确迁移流程，不自动改用新代码。

## 十 热重载

以下为规划合同。当前播放器尚未实现模块集合的热替换、story.register、state/node.state 或 game.restart；现有 game.start 的新会话切换与开发工具重新启动沿当前入口合同执行。

开发工具合并变更事件，编译并验证候选模块；失败显示源码诊断，继续运行原版本。新版本只在安全点提交。资源也先准备新版本，再替换引用。[8]
发布更改模块及其全部反向依赖的新 ModuleSet，固定各实例的导入绑定。候选初始化只可修改候选对象，宿主副作用要求 InitSafe；失败保留旧会话与全局设置。普通调用及当前剧情继续使用原模块集合。game.start 接收模块级零必需参数函数的稳定符号身份，独立于可选的 story 注册表，在替换会话前于最新已发布集合验证符号与签名，然后创建全新的模块全局。game.restart(story_id) 使用当前会话的主剧情模块集合并保留其变量。显式组件重建可采用新集合和调用者传入的旧模型；组件私有全局采用新实例初始化值。组件依稳定 key 和局部数据重建，替换订阅后释放旧订阅。拖动中更新推迟至手势结束或明确取消，业务状态与控件瞬态分别处理。
切换当前剧情属于显式开发操作，按命名段落与交互点检查状态结构；从段落入口重跑必须提示会重新执行该段动作。普通读档和回滚始终恢复完整快照。候选组件构造沿用初始化副作用限制：可分配候选节点和模型，只读已有业务对象；修改现有业务对象或启动外部操作会在构造处报错。候选模块与舞台替换完整准备后统一提交，失败保留旧版本。开发热重载不承担任意旧发行存档的自动迁移。

## 十一 完整制作能力与扩展
最终能力以 Ren’Py 的制作能力覆盖并进一步提升组合自由度为目标。本节同时列出当前合同与规划方向。拖放、可注册文本标签、TTS、3D Mesh/Material、custom_node、Live2D、音频滤镜/同步组、隔离回放、回滚/前滚和平台购买属于规划；其余能力的具体实现状态以对应章节为准。构建 feature 用于发行裁剪。每项必须有真实接口、对应实现和作者用例，统一节点或扩展接口的存在本身只是承载机制。
剧情阅读：ADV、NVL、气泡及多角色发言共用一个 DialogueInteraction。character.line 创建纯对白数据，say(lines) 把一组行提交到 game.dialogue.lines 并共享一次推进；character 的普通 _call 是单行快捷调用。当前每行保存角色、文本与揭示范围；逐行语音和位置属于后续扩展，NVL 可累积到显式清页。历史记录保存行组与内容身份。文本尚未揭示完时推进先完成本组，随后推进结束等待。回放使用隔离会话，退出恢复暂停的原会话；全局持久写入默认只读，作者需显式选择允许。
UI 交互：按钮、图片按钮、滑条、输入框、滚动、虚拟列表/网格、热点、拖放、工具提示、快捷键与模态。拖放有 DragId，记录源、载荷、捕获、候选目标与接受条件；按下/移动/释放/取消分别交付一次，失焦或 owner 撤销转 Cancelled，落点以同一呈现几何计算。文本输入区分编辑值、选区与 IME 组合串，组合期间保持平台候选框定位。组件公开语义角色、名称、值、操作与焦点顺序。
文字：文本服务接入 Parley 和字体回退，统一布局普通文本、样式 span、内嵌图片、链接、ruby 与双向/CJK 文本。富文本解析产生有源码位置的结构 span，文本标签/处理器可由作者注册。reveal 按字素映射到完整布局 cluster，速度/暂停标记属于交互时间线，快速完成保留最终排版。文字效果取得字形/cluster 标识与纯参数，复用 Effect 管线。
本地化：`tr(source, translations: null, params: null, locale: null)` 将原文和语言到译文的静态 table 写在同一调用中，返回普通 string。编译器按绑定身份识别 Reel 的 tr，宿主通过通用调用展开生成内部消息键、原文及参数；原文/译文包的内容决定身份。插值使用现有 Aside 格式化，原文表达式按书写顺序各求值一次，译文重用这些结果。译文可以重排、重复或省略原文中的值；存在歧义时在原表达式处报告，作者可以用局部绑定明确对应关系。字面模板配合显式 params 保留 Fluent 数值选择能力。

编译产物统一携带 CatalogProducts；源码会话消费产物并共享未解析的译文文本，按需解析所选语言。build/export 将生成目录写入既有文本归档，发布播放器按语言索引读取。Release 移除源码及编辑信息后，未选译文留在归档；Debug 保留完整作者源码。LSP 复用同一只读提取与验证逻辑，检查语言标签、插值对应和调用形式。 编辑器缓存同一语义快照产生的诊断、原文与译文预览及语种补全；插值表达式继续使用普通名称和类型查询。

`localization.set_locale(locale)` 准备目标语言及其可用的基础语言，成功后一起提交目录与可观察选择；失败或取消保持原状态。`localization.load(locale)` 显式预热并保留目录，带 source 时使用指定资源。查询依次使用精确语言、基础语言和原文。UI 的 derive 订阅语言变化，普通调用、已提交对白和历史保留已有字符串。目录解析和静态模板缓存归同一本地化服务，稳态读取复用现有数据。

可访问性：统一语义树驱动键盘/手柄导航、屏幕阅读器、TTS 与剪贴板辅助输出。作者设置替代文本、分组和朗读顺序；TTS 朗读当前已提交交互，与语音策略和自动推进服务协调。字号、对比和揭示速度可作为设置模型绑定，布局按实际文本尺寸更新。
图像与二维演出：分层立绘由条件资源子节点与共享参数组成；变换、遮罩、颜色矩阵、曲线、顺序/并行动作和粒子/精灵节点共用活动协议。stage.transition 保留旧目标的绘制数据，新目标继续由当前舞台生成；取消保留当前作者舞台并释放过渡资源，完整规则见“保留画面的舞台转场”。差分选择与动态资源版本都进入保存状态。
节点图像遮罩使用公共属性 `mask: null|string|MaskExpression`、`mask_progress: 1`、`mask_softness: 0.06`、`mask_invert: false`、`mask_x: 0`、`mask_y: 0`。字符串快捷形式使用普通 PNG/JPEG 资源路径，按节点局部布局矩形拉伸覆盖整个子树；不接受分块 .reel-image。progress 在 0..1，softness 为有限非负数，两个数值可派生及动画。字符串形式在 0 全隐藏、1 全显示并跳过额外遮罩合成；中间覆盖率为 alpha × clamp((progress × (1 + softness) − gray) / softness, 0, 1)，softness=0 使用硬阈值。gray 使用素材编码灰度而非线性化后的色值，invert 对 gray 取 1−gray。遮罩不更改布局、裁剪或逻辑命中，交互使用 visible/enabled 显式控制。组合表达式的构造、采样与参数见[可组合图像蒙版表达式](#可组合图像蒙版表达式)。VM 保存表达式图、资源路径与普通属性，恢复复用图片服务；无 GPU 句柄或截图进入脚本状态。渲染使用具名 Rust 元数据传递遮罩参数，复用帧资源租约和组离屏目标池，不逐帧解码、上传或 CPU 读回。
透视与可编程渲染：场景组可带 Camera 与 3D Transform，RenderFrame 支持版本化 Mesh、Material、纹理、uniform、混合/深度设置及效果输入。普通节点生成二维平面网格，透视后仍参与统一组顺序；显式深度组使用自己的深度缓冲规则。交互几何可以是二维逆变换、投影平面射线映射或注册的自定义映射。着色器改变形状时作者提供匹配命中策略。WGSL 是默认后端实现语言，Effect/Material 身份和参数独立于后端。
作者自定义节点：custom_node 注册可保存 state、measure(constraints)、record(draw_context)、event(event_context) 与显式重绘请求。绘制回调记录有资源身份的绘制命令，布局与绘制阶段只读并保持有界；异步加载由正常任务完成后标脏。作者可以增加小游戏、图形和输入行为而沿用同一布局、变换、焦点与恢复协议。Live2D 通过格式/SDK 适配节点提供模型、动作、表情及可恢复参数，SDK 许可与目标构建随适配器明确列出。
共同节点的 `blend: "normal" | "add" | "multiply" | "screen"` 对完成的整个子树与后方画面合成，默认 normal；构造、赋值、绑定和同包快照共用属性 schema。opacity、反色与遮罩先作用于子树，再执行混合；布局和命中不变。颜色运算在线性预乘 RGBA 中进行：add 对颜色和 alpha 的和分别限到 1；multiply/screen 使用透明度正确的 source-over 混合。复用现有绘制组和目标池，normal 路径保持原成本；其余模式用一个按现有预算租用的背景副本，不在逐帧创建 shader/pipeline。

节点 `invert: 0..1` 对完成合成的整个子树做 sRGB 反色，默认 0；1 将非预乘的编码 RGB 变为 1−RGB，中间值在编码 RGB 中插值，alpha 保持不变，再转回线性预乘值。嵌套节点按树由内到外应用，兄弟节点不受影响；反色先于该组的遮罩与透明度，不改变布局和输入。沿用普通属性写入、derive、tween 与快照，不新增语言语法。UI 的具名组元数据将参数传到帧与 GPU，在既有组离屏目标上一次合成，和 opacity/mask 共用目标；默认值不产生额外组，无截图、CPU 读回或新依赖。验证覆盖组合后的像素、半透明、嵌套与兄弟、遮罩、动态属性和保存恢复，并按设备标明像素验证范围。可运行示例为 examples/author_invert/src/main.aside。

音频视频：音轨可排队、定区间循环、淡化、声像、滤镜与同步组起播；语音支持逐句关联和历史重播。各操作对应可恢复媒体描述，队列/滤镜状态与逻辑位置进入快照。视频支持普通节点、全屏、循环、跳过、seek、音画同步与遮罩。错误、设备恢复和取消遵守媒体身份与唯一终态。
存档与历史控制：普通/快速/自动槽位、回滚、前滚、选择锁定、回放和持久解锁共用服务。前滚只复用仍匹配代码/状态版本的已记录交互与选择；分支改变使后续记录失效。作者可建立显式回滚边界。发行升级保存代码版本清单，构建工具保留所需历史编译产物；迁移在隔离候选中验证后切换，运行位置只通过声明的恢复映射改变。
平台与发行：桌面 Windows/Linux/macOS、Android/iOS/Web 以及额外 PS Vita 目标分别实现窗口、输入、音频、存储与生命周期适配。发布清单记录能力与后备策略，实际支持按平台验证。移动签名与 SDK、商店服务和受限平台编码能力由对应构建适配器处理。
外部服务：文件、HTTP、下载、更新、成就和平台购买使用版本化宿主服务。读取可按描述重发；外部写入带请求身份、幂等键及结果记录，保存/回滚遵守已定义的进程写恢复规则。native 模块默认静态注册，支持动态链接的平台可用版本化 C ABI；句柄为不透明 ID，资源与错误均有明确释放规则。生产发行能力与权限在构建时声明。
工具：当前 reel check/dev/build/export/run 提供检查、资源组织与构建；以下交互调试器、通用测试命令和演出编辑能力属于规划。节点、样式、属性绑定、资源、活动及任务可检查；工具模式的表达式默认纯读。测试驱动输入、截图及行为结果可自动对照，演出编辑辅助输出可维护源码。语言 LSP 与 debugger 使用同一签名和源映射元数据。

`reel-lsp` 将 Reel 注册的默认环境、宿主声明和属性文档提供给 `aside-lsp`，采用 stdio 启动。VS Code 客户端负责文件关联、高亮、进程管理及只读内置定义的显示。内置类型和方法的编辑信息随 Rust 声明维护，具名参数与默认值来自运行时注册；共同节点属性逐项参与补全。编辑查询只读取源码和声明，不创建游戏会话。

宿主名义类型沿源码、lowering 和 VM 使用同一份可信登记。VM 为每个注册类型保存共享祖先描述，宿主值在既有私有槽中保存类型索引；节点数量增加时只保留各自的索引。注册和描述内存进入代码容量统计，类型检查计入执行预算。快照保存类型身份及共享登记指纹，恢复时核对当前登记。

### 可组合图像蒙版表达式

`mask.alpha(source, x:0, y:0, scale_x:1, scale_y:1, repeat:"none")` 和 `mask.gray(...)` 返回不可变标量表达式，按节点局部矩形归一化 UV 采样 PNG/JPEG；UV 为原坐标乘 scale 后加偏移，repeat 为 none/x/y/both，未重复轴夹到纹理边缘。alpha 读取线性 alpha；gray 将已采样的线性预乘 RGB 还原为编码 sRGB 后按 0.2126/0.7152/0.0722 加权，透明样本返回 0。图片通过现有资源服务加载和退休。UV 参数接受数值或表达式，负 scale 可以翻转。

`mask.param(name)` 读取所属节点的 progress/softness/x/y，分别对应 mask_progress/mask_softness/mask_x/mask_y。新增 mask_x/mask_y 为有限可表示 f32 的数值，默认 0，使用普通赋值、绑定和 tween；原 progress/softness 约束不变。`mask.add/subtract/multiply(a,b)`、`mask.pow(value,power)`、`mask.clamp(value,min:0,max:1)`、`mask.smoothstep(edge0,edge1,value)` 组合标量。普通有限可表示 f32 数值可作为输入；pow 指数为有限非负常数，底数先夹到 0，0^0=1；clamp 的常量边界有限且有序。smoothstep 倒序边界给出反向过渡，相等时按 value>=edge 的硬阶跃。最后覆盖率夹到 0..1，再乘子树透明度；不隐式乘任一输入 alpha。

表达式只包含上述纯原语，没有作者 shader 字符串、循环、IO 或脚本回调。构造边界验证类型和常量，挂载时编译并共享不可变 DAG；参数变更只更新数值，不复制像素、重建表达式或编译管线。字符串 mask 在同一入口降低为已有灰度阈值语义，保留 0/1 端点、softness=0 硬边和 mask_invert；仅该快捷形式的 progress=1 跳过合成。表达式始终按其结果执行。mask=null 解除蒙版；布局、逻辑命中和普通节点变换保持共同舞台规则。

执行选用结构编译的单个覆盖函数，避免每像素解释循环和固定操作数上限；GPU 管线按表达式结构/质量/设备缓存，缓存只弱持有程序，退休程序清理相应登记。纹理资源按设备采样绑定上限准入；VM 表达式和投影构建工作使用已有堆预算；投影完成后的 CPU DAG 沿用源 Scene/RenderFrame 的共享驻留模型，不宣称跨线程镜像受 VM 堆硬上限约束。生成代码、渲染结构副本、缓存登记和纹理准入使用既有 renderer resources 账目；驱动私有编译空间单列。共享 DAG 子表达式只生成一次。整段蒙版和普通子树组复用现有离屏目标，不逐原语分配纹理。

检查源码/ASB、动画/绑定、快照、隐藏/替换/退休、单图旧语义及双图实际 GPU 像素，测量暖帧上传和管线复用。公开示例 [author_composed_mask.aside](../examples/author_composed_mask/src/main.aside) 使用项目自带的两张 alpha 图片。

## 十二 字节码与资源分包发布

### 构建与导出

作者工具以平台二进制包分发，`bin` 包含 `reel`、`reel-lsp` 和 `reel-game`，多个项目通过 PATH 共用。项目入口固定为 `src/main.aside` 中的 `main`，资源放在 `resources/`，生成产物放在 `build/`。check/dev/build/export 从当前目录及祖先定位项目，直接读取固定入口；作者复用源码时复制到 src，通过普通 use 导入。CLI 和 Reel LSP 共用此源码根，项目模块由入口实际导入发现。资源目录内的相对路径就是脚本使用的资源路径。

`reel check` 检查项目，`reel dev` 编译并运行源码。`reel build` 默认写入项目的 `build/debug/game.asb`，`--release` 写入 `build/release/game.asb`。`reel export` 默认写入 `build/export/debug/`，`--release` 写入 `build/export/release/`。默认产物可重复构建替换；显式 `--output` 保留覆写选择。项目入口无源码位置、函数名或外部模块根参数；指定源码的编译和检查仍可通过编译器库供宿主及验证工具使用。

`reel fmt` 格式化当前项目 `src/` 中全部 Aside 文件，`--check` 只报告排版差异并以非零状态退出。CLI 和 LSP 共用 aside-compiler 的语法树打印器；CLI 先完成全部候选文件的解析与格式化，再逐文件原子发布修改，语法失败保留原文件。LSP 使用未保存文档及缓存的 Parse，已符合排版时返回空编辑。VS Code 的 Aside 语言默认开启保存时格式化，作者可在编辑器设置中调整。

语言服务的补全、悬停、导航和语义着色共享编译器的文档快照。语义 token 使用标准名称分类和协商后的坐标编码；Reel 的内置类型与成员继续取自宿主声明。VS Code 客户端提供词法着色和声明片段，语言分析由 Rust 服务提供。

默认导出通过资源根挂载保留逻辑路径；显式 `--resource` 或 `--text` 使用作者给出的挂载集。`--resource .=directory` 将目录内容直接映射到资源根。导出只复制已解析模块和声明资源，允许产物位于项目的 `build/`，检查输出是否包含实际输入文件或与资源目录重叠。源码与原资源始终保留在项目目录。

`reel run` 省略路径时读取当前项目的 `build/debug/game.asb`，`--release` 选择 `build/release/game.asb`；显式 ASB 路径与该 profile 选择互斥。无分发清单的 ASB 沿所在项目定位 `resources/`；带分发清单的包继续使用清单自己的资源位置。显式 `--assets` 优先。

源码翻译随统一项目编译生成；check 验证而不写文件，dev 使用源码目录服务，build/export 生成语言索引和文本包。Distribution 保存语言到包内逻辑键的索引，播放器将其交给现有 CatalogLoader。语言数据使用既有 archive 分发；build 的语言包与 ASB 相邻，发布更新保持旧 ASB 所引用的内容可用。

`localization.set_locale(locale)` 按需准备目标语言及其可用的基础语言，成功后统一提交目录、当前语言和响应式通知；缺少对应译文时保留原文回退。读取失败或调用任务取消保留旧状态。新的有效切换请求撤销旧请求的候选；仍存活的旧调用返回 null，语言选择由新请求完成。`localization.load(locale, source: null)` 为显式预热，省略 source 时按语言索引定位；显式 source 继续支持作者选择的资源。自动选择的目录只保留当前及基础语言，显式预热的目录保留供 `tr(locale: ...)` 等同时使用多语言的场景。旧语言的模板缓存随自动目录释放；切换提交期间允许旧目录和候选目录同时存在。单语言作品直接使用原文。

`reel build --output <file.asb>` 生成可独立加载的代码包。默认 Debug 保留完整源码、指令源码映射和声明位置；`--release` 剥离这些调试数据，保留运行时符号、结构与实际使用的常量。两者共用编译、验证与执行语义。Release 故障报告错误类型、函数与指令位置，不伪造源码行列。执行快照核对实际代码身份，不跨不同产物迁移。

`reel export [--output <directory>] [--release] [--resource prefix=directory] [--text prefix=directory]` 使用同一项目编译入口，组织可搬移的工程或发行目录。开发导出包含 Debug `game.asb`、`src/` 中的已导入源码、`resources/` 中的散资源；进入导出目录可直接 reel dev。发布导出包含 `game.exe`（其他平台为对应可执行文件）、`game.asb`、`game.rtp` 和资源卷。两者均可 `reel run <game.asb>`。文本和资源挂载前缀保持脚本逻辑路径。

发布播放器为 reel-cli 包内启用 window feature 的 `reel-game` 薄入口，与 CLI 共用 ASB 加载及窗口启动路径。export 从当前 CLI 同级复制该平台播放器为 game；Windows 入口使用 GUI subsystem。无参启动从播放器所在目录定位 `game.asb`，默认存档为同级 `saves/`，显式 `--save-directory` 可覆盖；工作目录不影响主包定位。普通 CLI run 保留原有存档目录策略。`cargo build --release -p reel-cli --features window` 同时构建工具和播放器，缺失播放器时发布导出提示安装包含播放器的完整工具包。

资源按逻辑路径排序，以完整资源为边界分卷；默认目标为 2 GiB（含包头和索引），发布参数 `--pack-bytes <非零字节数>` 可调整。第一卷为 `resources.rpk`，后续为 `resources-002.rpk` 等，由内嵌清单按顺序挂载。单个资源超过目标时独占一卷。

清单位于 ASB 的 Distribution 区段，保存相对主 ASB 目录的可空 text、按覆盖顺序排列的 resources、可空 loose 和 catalogs 语言索引。loose 为 `Project` 时按主包位置寻找源码项目的资源根，为 `Directory(path)` 时使用相对主包目录的路径。加载在同次容器解码取得代码与清单，验证路径后绑定文本池；图片、字体、翻译和媒体统一交给 AssetIo。开发启动与导出覆盖检查通过 Read+Seek 只读取头、索引及受 Limits 约束并校验摘要的清单。发布默认 loose 为 null；显式 `run --assets <directory>` 开启覆盖。散文件只在 NotFound 时回退包，包按声明顺序后者覆盖前者。普通 build 无翻译时可省略清单，资源根按所在项目定位。

导出在输出目录同级创建候选目录，全部文件成功后发布，失败保留原导出。已有目标须包含有效 Reel 清单；替换的是整个导出，不复制旧存档或其他附加文件。输出可位于项目的 build 目录，须与实际输入源码文件和资源目录分离。目录切换失败时恢复旧目录；并发运行中的热更新与断电恢复属于后续能力。普通 build 的单文件发布仍复用 reel-storage；CLI 参数解析用 clap，候选目录生命周期用 tempfile。

### ASB 声明映射验证边界

reader 从已验证 Aside 字节码 的 GlobalDef.linked_function 恢复具名模块函数，核对模块、名称、不可变 global、无捕获及唯一声明映射。具名函数在顶层执行前完成链接，支持合法的函数循环引用。一次程序扫描禁止代码初始化或改写这些声明 global；工程 wrapper 的调用顺序与全部源码初始化器逐项核对。扫描和有序表操作计入工作额度。

声明映射来自实际链接绑定和源码映射，嵌套函数不能仅靠同名元数据成为模块入口。编译器更改声明结构时，同步 reader 规则与对应行为验收。

### 代码容器与文本池

作者编译与 ASB/Aside 字节码 加载默认仅受格式索引宽度、可寻址内存与实际分配结果约束，不以预设作品字节数、累计记录数、模块数或指令数截断正常作品。Rust 宿主仍可显式设置各阶段 Limits 配额；解码长度、索引、结构与整数溢出检查保留。递归深度及诊断条数继续限制递归实现的栈与错误恢复，不冒充作品容量。

ASB 保存已经链接的完整模块图、初始化顺序与实际入口。Aside 字节码是代码、常量、函数、导入导出、类型与 native 合同的唯一来源；容器元数据不复制另一套执行模型。容器由 reel-package 定义，编译器 HIR 到便携元数据的转换由 reel-cli 完成。模块/函数索引在验证时准备，运行时不扫描模块表。

ASB 当前格式为 1.0，加载器检查容器版本。容器头固定 72 字节：magic[8]=REELASB 加零字节、major:u16、minor:u16、flags:u32、total_len:u64、index_offset:u64、index_len:u64、index_blake3[32]。整数小端，偏移为文件绝对位置。索引项为 kind:u32、flags:u32、offset:u64、len:u64、blake3[32]；必需且唯一的区段为字节码与 UTF-8 JSON 元数据；导出另含唯一的 kind=3 UTF-8 JSON Distribution 区段，该区段存在时标记 required。读取检查范围、重叠、摘要与结构，未知必需语义拒绝。只读写当前格式，不保留旧格式解码分支。摘要用于完整性与身份，不授予宿主权限。

元数据保存规范模块路径、源码版本、声明/绑定/global 映射、初始化顺序、initializer 与可空 game_entry。可空 debug 记录保存源码描述与声明位置；Release 中为 null，字节码 sources/source_map 为空。读取端交叉验证模块归属、索引唯一性、实际入口及精确字节码摘要。check/build/export/dev 使用 src/main.aside 中无必需参数的模块级 main 函数。消费者完成工程初始化后启动入口。

发布导出把字符串常量替换为文本池 offset:u64/len:u32，字节码保存 TextPoolDescriptor 的总长度与摘要。原始 UTF-8 池在 `game.rtp` 的 `__reel/text.pool`；宿主读取后绑定并检查长度、摘要和字符边界。池身份参与代码身份，归档里的译文修改不改变它。Aside 只接收共享不可变内存，不读取文件或依赖 Reel 包格式。

VM 按实际使用物化字符串与 tuple；字符串视图共享程序中的正文，弱常量句柄表不保活已丢弃对象。Debug 内嵌文字也走同一共享读取路径，不在启动时把所有正文复制到永久 GC 根。常量文本快照保存常量引用，动态生成的文本保存实际内容。当前原文池整体驻留，已加载的 Fluent 目录仍整体解析；磁盘分块文本缓存尚未接入，不能把外置文件等同于全文不驻留。

### 资源归档与范围读取

`.rpk` 和 `.rtp` 使用同一种索引归档；kind 区分资源与文本，payload 原样存储，不整包压缩或加密。头为 32 字节：magic[8]=REELPAK 加零字节、format:u32、kind:u32、index_offset:u64、entry_count:u64。尾部索引每条记录为 path_len:u32、offset:u64、length:u64、content_digest[32] 与 UTF-8 路径。路径是区分大小写的逻辑相对路径，单包不允许重复。构建边复制边计算条目摘要，使用固定大小 scratch，不把资源整包读入内存。

运行启动只加载索引与打开包句柄。PackIo 与 DirectoryIo 共用有界 worker、取消、范围读取和完成票据；包条目直接 seek 到 offset+请求偏移，不重新哈希整包。视频 Read/Seek 也经过同一资源桥，音频实时线程不读取归档。内存、IO、解码与上传预算由宿主配置，与作品总文件大小无固定关系。

## 十三 性能目标与可观测性
固定 720p、1080p 验证场景记录冷启动、包加载、输入响应、CPU/GPU 帧时间、分配、内存与纹理峰值。首阶段以 Windows x64 作低端体验基准，Linux 与 macOS 各自验证窗口、输入、渲染、音频和持久化。低端参考档为 N100 级集成显卡、8 GiB 内存、720p；每份结果注明实际硬件与构建配置。
目标为控件可见响应 p95 小于 50ms，基础动画 CPU 帧工作 p95 小于 8ms，GPU p95 小于 12ms，呈现间隔 p99 小于 33.3ms。60Hz 稳态同时要求间隔中位数小于 17.5ms、超过 25ms 的比例低于 1%。这些均待实测验证。
主线程起始调度配额为脚本 2ms、后台结果与资源安装 1ms、回收 0.5ms、快照捕获 1ms；VM 至少每 256 个工作单位检查时间，分配与释放另计。单次不可分割操作同样测量，超限不能只靠总额度掩盖。GPU 上传按帧字节预算分批。快照暂停造成的业务排队时间单独记录，小场景目标一帧内完成。
资源预算起点为 VM 堆 32 MiB、CPU 解码驻留 64 MiB、GPU 纹理 128 MiB、桌面离屏工作目标 128 MiB；峰值包括待释放对象、候选模块和快照副本。超限先逐出可选缓存、延后预取，再报告具体资源需求。队列与回收积压纳入背压，不能无限增长。
每阶段必验：静止场景无无效脚本、布局和呈现；单滑条不更新无关布局；纯音量变化只通知音频；单节点淡化复用排版。开发运行和 release 都测体验，优化由同一工作负载的前后数据证明。

## 十四 开发顺序与验收

调度与清理行为检查为 `cargo test --locked -p aside-vm --test gc_scheduling` 和 `cargo test --locked -p reel-player --test runtime_cost`。CPU 成本测量为 `cargo test --locked --release -p reel-player --test runtime_cost runtime_cost -- --ignored --nocapture --test-threads=1`，`REEL_COST_ROWS` 控制常驻数据行数，默认 10000。输出分别记录脚本更新、定时唤醒、捕获及候选恢复；快照内存是逐片观察的新增存活分配，不是进程 RSS 或瞬时分配峰值。窗口、字体、渲染、磁盘和设备成本需另外测量。
当前能力与剩余工作集中在[文档入口](README.md#当前状态)和[引擎总清单 #94](https://github.com/jhq223/reel/issues/94)。实施按作者可用的成果推进：组件表达与常用控件、演出与资源起点、长内容运行成本、同包恢复及设备覆盖。语言和标准库沿同一能力清单补齐，已实现的子集与完整目标分别标注。每次变更以现有公共舞台、所有权和资源账本为基础，同步接口与消费者。
验收采用最小宿主夹具和针对性 UI 片段，不要求制作完整可玩流程。NullPlatform、记录型渲染器、假时钟和内存 IO 验证无隐式设备、时间或文件系统依赖；关闭 winit/wgpu 后检查核心及传递依赖。用例覆盖单次输入消费、配对按下与释放、拖动失焦、模态隔离、嵌套合成、保存后切场景、读档后旧结果、重载已有回调及节点销毁后的异步完成。音频、恢复与帧预算分别提供可重复的负载和观察值。协作执行器测单步最大耗时、满队列、取消与过期完成；表面丢失及挂起恢复测时钟和资源身份。Vita 承诺前先做 32 位交叉构建，再依次验证启动、输入、图片/字形、音频、可靠保存与效果，不承诺未经实测的帧率。
每个代理任务写明目标、允许目录、冻结的接口版本、已有代码、禁止扩大的范围、行为场景与性能预算。Issue 按可验收成果组织，PR 保持独立可审查；提交包含实际改动、相关用例、运行命令、观察结果与未解决项。不得以测试数量、模拟成功或完成报告代替实际契约验证。接口或依赖变更先提交设计修改，经负责人确认后继续。负责人亲自审输入、所有权、保存、音频和平台关键路径，重跑关键验证并处理跨模块冲突。性能计数器从第一阶段建立，协议变更同步三份设计。各后端的构建、设备和尾延迟验证属于对应实现任务。代码与文档以同一能力清单记录结果。

## 十五 UI 模块与呈现桥接合同

舞台指针（已实现）：`stage.pointer()` 返回最近真实鼠标输入在已接受逻辑舞台中的 `{x,y}` 快照；无接受画面、离窗、失焦或触摸/键盘手柄导航时返回 null。位置不随执行快照恢复，返回的普通数据值照常保存。`await stage.move_pointer(x,y,owner:node)` 请求真实宿主定位；owner 必须是挂载、可用且在当前模态范围内的节点。请求归该节点所有，与窗口配置共用单个有界在途槽，取消/会话替换撤销未执行请求。宿主按接受的舞台变换与 DPI 转换，目标须在客户区；当前输入/呈现/节点无效或不支持返回 false，执行成功经普通 Move 更新命中与悬停。非法参数及宿主执行错误给出诊断。能力为 `window.info().capabilities.pointer_move`，定位句柄留在平台适配器。检查源码/ASB、取消/恢复、接受坐标、模态/隐藏/输入方式；`cargo test --locked -p reel-engine --lib aside::tests::window`、`cargo test --locked -p reel-player --lib pointer_` 与 `cargo test --locked -p reel-cli --test author_pointer` 可复跑。坐标检查覆盖非单位舞台缩放、DPI、黑边和未接受的新布局；实际系统定位仍按设备分别验收。
本节规定 reel-ui 的模块与行为边界。UI 结构整合不改变已验收的控件、CPU 帧记录、呈现几何和资源契约；后端与设备仍按各自用例验收。工作区采用 Rust 1.96.0 工具链。

自定义光标（已实现）
`cursor(children: [], ...)` 是普通舞台容器，挂载父节点拥有其寿命。整个子树不参与指针、焦点或父容器流布局；可用图片、文字、变换、透明度、遮罩和普通活动构成外观。容器局部原点跟随鼠标，x/y 是相对热点的偏移；子 image 的 anchor 或 x/y 定义图像热点。外观使用窗口逻辑坐标，在完整场景之后绘制，祖先相机、变换、裁剪和合成不影响此窗口层；cursor 本身及后代的合成仍沿唯一绘制流和资源预算。最近鼠标输入的接受命中路径决定 owner；禁用控件不接收 hover，但仍可选择该路径上可用的祖先光标，按压捕获期间使用捕获路径；从深到浅选首个具有可用 cursor 的 owner，同父以树顺序最后者优先。祖先 visibility/enabled、退休和当前模态范围决定资格，旧接受帧不能复活已隐藏或退休的节点。可见空容器隐藏系统光标；visible=false 或移除让回祖先 cursor 或系统光标。离窗、失焦、切换触摸、清空接受视图和会话切换撤销自定义光标；再次真实鼠标输入按当前画面恢复。指针坐标和平台句柄不保存，普通节点、资源和动画沿现有快照恢复，恢复后等待真实指针输入。移动只更新 UI 呈现位置，不回写 VM 坐标、不增加脚本逐帧任务，也不重新排版未变外观。检查入口为 `cargo test --locked -p reel-cli --test author_cursor`、`cargo test --locked -p reel-cli --test author_images cursor_source` 和 `cargo test --locked -p reel-player --features window --lib cursor_`。GPU 像素检查加 `cursor_offscreen -- --ignored --nocapture`；隐藏原生窗口检查为 `actual_cursor_window -- --ignored --nocapture`，需设置真实字体路径 `REEL_SESSION_FONT`。原生检查使用宿主生成的 move/leave 事件与实际 GPU 表面，不代表物理鼠标、触屏或各平台图形体验已经验收。示例：[author_cursor.aside](../examples/author_cursor/src/main.aside)。

呈现几何与上下文
reel-ui::app/core 导出几何、选择输入源并分发事件；bridge 管理 FrameId、surface epoch、呈现接受门和 NodeId↔WidgetId 的代际映射，沿用 reel-core 的共享预算。输入源明确为 Live 或 Presented，Presented 内可尚无接受视图。Reel 使用 Presented；首帧前几何命中为空，宿主关闭/挂起等平台命令照常处理。
几何快照包含全部登记节点，包括 stashed 节点。采用连续记录与 children 索引数组，每节点保存 WidgetId、parent 索引、children 范围、对齐 border-box 尺寸、border/paint insets、局部变换、border-box 到 window 变换、window 子树 bounds、已解析 content baselines、border-box 坐标的 clip 和 frozen 命中标志（content-box getter 只转换一次）。整帧保存 DPI、根及 id 索引。布局缓存、属性、action 和完整 WidgetState 留在当前运行时。路径沿 parent links 在有上限的复用缓冲中生成一次，整次事件借用；内存与构建成本计入候选帧预算。
每次真实指针分发固定一个 view，命中、捕获、祖先路由、hover、cursor 和所有输入上下文共用它。Context 借用 view 与已验证记录索引，QueryCtx 保持可复制；每个 context 无需克隆 Arc。自身几何 getter 的前置条件由构造入口保证。查询缺失或已销毁子节点时，QueryCtx::get、get_raw/get_raw_mut 等入口返回 Option/Result；Presented 缺失记录保持缺失，既不回退 Live，也不按当前控件几何重新命中。WidgetRef::children 依据所选拓扑枚举并过滤已销毁节点。布局、动画、普通 mutation 以及非指针状态更新显式使用 Live。
事件、更新、查询、控件引用和动画上下文共用同一输入几何来源。当前存活、stashed、disabled 状态继续即时生效；disabled 命中保留祖先消费路径，自身行为跳过。捕获失效与普通命中的 disabled 处理分别判断。
接受新视图仅发送实际路径差异的通知。需要退出的 hover/active 状态及失效捕获使用旧视图完成清理，再用新视图处理进入和静止 cursor；仍有效且在新视图中的捕获继续。清空 Presented 或切换表面代数时保留旧视图直到必要退出结束。Up/Cancel 在捕获目标回调开始时记录终止已交付，回调期间仍可观察捕获状态，删除与祖先冒泡只清理一次。

统一记录流
默认记录路径直接访问 VisualLayerPlan 的完整有序片段（§16）。Reel 导出在同一流中保留 Draw、Push/PopGroup、Push/PopClip 和诊断上下文；Scene 局部 ID 与源身份一起映射。效果组在 End 时整体合成一次，跨记录片段保持组与 clip 作用域。Imaging 图片、字体和 blob 转成带版本与预算的 Reel 资源引用，几何与绘制记录属于同一候选帧。replay-to-Scene 仅保留为显式调用的结构/CPU 参考。
独立片段是画序分段，消费时按原顺序组成完整流。使用稳定层边界时，paint_layer_mode 为 retained 声明：初始 Inline，缓存回调未执行时保留，显式设置 Inline 才恢复普通模式；其他 pre/paint/post 回调执行时，保留未重声明的层模式。
外部视频、材质或三维内容通过同一流的 ExternalDraw 接入。最小记录含宿主内容/资源 token、拥有 WidgetId、local bounds、content 到 layer 变换和确定画序位置；继承当前位置打开的 Group/Clip。其位置替换自身 paint 内容，保留 pre → own clip → external paint → children → pop clip → post。Reel 始终只有这一条画序来源，具体外部设备实现另行验收。

作者数值控件
`slider(value: 0, min: 0, max: 1, step: null, on_change: null, ...)` 与 `toggle(label: null, value: false, on_change: null, children: [], ...)` 使用同一 Stage 和共同外观属性。slider 复用 reel-ui Slider，toggle 复用 ButtonBehavior；开关可带标签和自定义子树。slider 的 min/max/value 必须有限，min < max 且跨度有限，value 在闭区间内；step 为 null 或有限正数。步进以 min 为锚点，两端始终可达。脚本可以设置任意区间内值；step 只约束用户输入。范围属性写入先检查完整当前状态，失败不修改原属性。
控件动作先更新唯一权威 `node.value`，再调度 `on_change` 的节点任务，事件包含 target 和此次 value。直接脚本赋值静默投影，不触发回调。闭包可等待并沿用节点退休取消；省略回调仍保存控件值。slider 默认水平轨道，color 控制活动轨道/滑块，background 仍是共同节点背景；toggle 以状态指示和可选标签呈现，children 按 stack 叠放。
Slider 统一绘制与指针逆映射的滑块中心端点，退化几何没有指针值。呈现几何记录控件交互配置版本；range/step 变化取消旧捕获，在对应帧被接受前阻止输入，value、尺寸和变换变化保留配置版本。指针使用接受帧的几何，键盘/AccessKit 从当前值增减。连续控件事件不以每次 value 更新的整个会话投影版本重置手势；已退休身份、当前可交互状态与范围仍在提交时检查。没有输入时不启动 VM 轮询任务。宿主可用 `SessionUi::handle_access` / `AsideSessionUi::handle_access` 投递 AccessKit ActionRequest，通过共同呈现几何、模态路由和回调入口；原生屏幕阅读器适配不由此 API 自动提供。

源码按职责组织为 reel-ui 内部模块，控件基础、树、呈现桥接不再拆成独立包。保留原始许可和必要版权/来源说明，普通外部依赖继续锁定引用；验证以当前行为、单一类型来源和依赖边界为依据，不做上游目录或补丁重放门槛。

## 十六 可移植绘制帧与默认光栅后端

### 帧与导出

reel-ui::frame 定义 RenderFrame，保存帧身份、SurfaceEpoch、逻辑/输出尺寸、确定画序、分组、裁剪、外部内容、诊断上下文和资源引用。绘制数据复用 kurbo/Peniko 的几何与画刷描述；字体记录直接使用 Parley 已计算的 glyph ID、位置、字号、字体集合索引及 variation 坐标。渲染器接收这些结果，排版仍由原有文本系统负责。

导出直接访问 VisualLayerPlan 的有序片段与 imaging Scene 的公开 command/clip/group/mask/draw/context 数据。Scene 内的 ID 只在所属源 Scene 中有意义，映射键包含源 Scene 身份及局部 ID；若变换烘焙进数据，还需区分重放实例。mask 定义优先保持局部坐标，使用处携带变换，通过按实际存储计账的映射转成帧内索引。多个片段共用连续的 group/clip/context 状态，mask 子流使用独立验证栈。imaging 对 clip/group 深度分别验证；可移植记录同时保留独立逻辑栈及原画序。跨片段的正常嵌套完整支持；交叉结束的 clip/group 寿命缺少已定义的像素语义，导出以带位置的 UnsupportedRecord 明确拒绝该模式，避免按另一种栈规则猜测结果。IsolatedScene 表示记录边界，其本身不额外产生透明组。默认生产路径省去先展平为第二份 Scene 的复制，replay-to-Scene 保留作结构参考。

RecordedSource 移入同次真实 VisualLayerPlan、几何和候选身份。FrameBuild 持有其冻结租约，跨暂停复用这一份源记录；完成或取消后释放源租约。step 按命令、字节和工作额度推进，保存片段、命令、嵌套 mask 以及路径段、glyph、渐变 stop、字符串字节等 payload 子游标；全部遍历使用按实际深度扩容并计账的显式栈。每次扩容、路径/glyph/渐变复制与映射建立均先申请额度。单片工作结束返回未完成状态，实际字节预算不足或内容错误返回定位失败；结果完整后公开不可变 RenderFrame。生产导出使用可提前结束的 visitor，避免 infallible PaintSink 在失败后仍走完整条记录。

构建中和等待提交的候选共用容量限制。取消、失败或表面代数失效释放对应候选资格；几何和数据的实际驻留仍由最后租约计费。构建、上传完成都保持旧接受几何；协调器仅把精确候选的 PresentAccepted 送入既有接受门。Accepted 通知独立持有候选资格和同次几何，直到 resolve 或最后一个通知副本释放；允许 render_present → 创建通知 → 释放调用方 frame → 图形完成 → resolve 的同步顺序，不为等待接受而保留整帧资源。普通 key/几何引用不延长资格，未提交的 source/build/frame 在无接受通知时释放仍取消；通知不能复活已取消的候选。epoch、身份和接受顺序检查继续执行。图形完成通知独立决定后端资源回收。

BuildLimits 仅保留 max_bytes。片段、绘制命令、mask 定义、嵌套深度和累计工作没有额外硬上限。主流和 mask 流按实际命令数保留存储；映射、作用域与 mask 遍历栈按需求扩容，分配前计入字节预算，退出 mask 后退还其游标暂存占用。StepBudget 继续限制每片命令、工作和字节，调用方按 minimum_units/minimum_bytes 允许下一不可分元素推进。

GeometryLimits 仅保留 max_bytes；几何导出使用显式 DFS，按实际登记节点及深度保留并计量快照和暂存。PresentationBridge 每次 record 在发布候选前按 geometry.max_depth 扩充输入路由，保留旧 accepted 几何和活动捕获。routing_storage_bytes 表示当前路由存储。BudgetTicket::resize 在同一账本原子调整保留票据，失败保持原占用；候选数量仍受真实等待队列容量约束，快照和路由仍保留实际字节预算。

### 资源身份与所有权

图片、字体和外部内容先在有界资源登记表中建立 ResourceVersion 与拥有型租约。复用 reel-core 的 AssetKey、ContentVersion，紧凑运行时句柄包含登记表代数及完整槽身份。源 Blob ID 只作为查找线索，登记时同时核对内容对象/版本，避免把碰撞身份当成相同内容。内容版本在资源准备阶段固定，导出只查表；缺少登记给出具体资源需求，由准备工作完成后重新构建候选。

登记表持有共享不可变数据，帧保存资源引用/租约，上传和在途提交各自取得其所需拥有权。同一图片或字体在多次绘制中复用数据。缓冲、索引表、候选副本、等待上传、图形在途与退休空间均计费；物理释放完成后退账。retire 停止取得新租约并移除登记表自己的保留；已有租约仍可用于提交，包括暂停构建、完整帧和已接受上传。全部租约释放且最后一次图形使用完成之后才归还实际存储，槽复用更新完整代数。导出的字节预算约束新增帧、映射与登记存储；工作额度只限制每个执行片，不限制整幅画面的累计工作。源 Scene 私有表容量、未被 command 引用的 mask 与共享 Blob 的底层分配属于生产者记录/缓存，公共 command visitor 只提供内容访问。R01 对这些源驻留分别测量 record、暂停构建、完成和取消时的实际分配/进程峰值及源清理耗时，明确标识源租约保留期；不以命令长度冒充精确源分配账本。需要进一步细化源驻留计账或分片析构时，增加真正的 owned-parts/drain/storage 接口后验证。

等待上传字节额度累计约束全部未完成批次的 payload 容量票据，与上传加驻留总额度独立检查；拒绝返回原批次。选定无强租约的资源槽后，先释放登记表自己的过期弱描述符票据再预留替代项，外部弱别名继续保留相应责任。

reel-render 定义后端及上传/呈现/完成/退休接口，读取 ui/core。RenderFrame 和资源描述保持平台无关；wgpu、窗口及平台句柄属于实现适配层。记录型/null 后端验证合同，提供明确的模拟提交结果；设备性能由实际后端测量。

### 外部内容与坐标

UI 外部层保留宿主不透明 token、拥有 WidgetId、内容局部 bounds 与 content-to-layer 变换。真实顺序固定为 pre-paint → own clip → external paint → children → pop clip → post-paint。外部 paint 前结束当前记录片段，继承已打开的组和裁剪；其后子节点继续同一顺序流。跳过缓存回调时保留上次声明的外部模式/token，显式切回 Inline 才恢复普通内容。

基底层与其他层使用一致的 layer-local-to-window 约定，绘制与 PresentedGeometryView 使用同次变换。内容坐标包含 border-box 的相应平移，clip 使用明确的 border-box 坐标。有限但奇异的变换可以保留视觉几何，输入逆映射返回空；导出转换使用显式上下文和前向变换，避免强制求逆。前向累计使用 UI 的完整 compose_local_transform，包含 layout origin 和 scroll translation；几何记录中单独的 transform 字段仅是其组成部分。非有限变换在变更提交/导出时给出定位错误。

paint/append 边界另存连续命令的实际绘制者范围，普通 Inline 子控件的命令和定位错误不能以片段边界 owner 代替；范围游标在已计费的命令提交/片段推进时只访问相邻范围，初始化以固定一次索引读取得首个 owner；下一命令完成映射预付款后发生错误时仍报告该命令的实际 owner，诊断不做未计费扫描或延后查找，payload 暂停继续保留相同来源。生成的 clip、调试框和检查器绘制也保存实际 owner。调试 bounding_box 已在窗口坐标中，使用 identity 片段；baseline 前向累计到窗口坐标，跨片段保持打开的作用域，不对奇异变换求逆。

组明确保存效果坐标空间、效果参数和保守 ink bounds。imaging Group 本身缺少整体效果变换，因此 paint/append 边界为组保存保证存在的 owner 与效果空间元数据；组的来源不能靠某个子绘制或可选诊断上下文猜测。bounds 包括笔画、字形外延、抗锯齿与滤镜扩张，布局盒仅用于布局。mask 自己的记录流、变换、alpha/亮度模式和采样规则随组保留。

### 默认后端方向

默认使用 wgpu 的常规几何与合成管线：图片四边形、缓存路径网格和字形图集四边形相邻兼容时合批。路径细分复用 lyon_tessellation，字形栅格化复用 Swash scale；保留现有 Parley 整形结果。固定依赖版本分别为 1.0.22、0.2.10。Swash 的 shaping 路径不进入此适配器。

字形填充、描边与字体合成变换共用 Swash 轮廓和其 Zeno 光栅器。栅格前按最终笔画边界检查纹理尺寸和资源预算；图集键包含字体、字号、变体、字形、填充/描边参数及字形变换，颜色和位置变化复用覆盖率。轮廓及光栅工作空间复用，提交、放弃帧与字体退休沿既有图集租约释放。

颜色在合成前转为线性并预乘 alpha，输出到表面时按 SurfaceInfo 编码一次。图片准备按源编码/alpha 描述转换；纹理过滤采用线性预乘数据。渐变保留声明的插值规则，产生的采样颜色进入同一线性合成约定。灰度字形覆盖率与颜色分开，彩色字体图像按自己的编码转换。

普通绘制直接进入当前目标；需要独立组结果、mask 或 transition 时才使用离屏目标。EndGroup 对完整组结果应用一次 mask 与组透明度。alpha mask 使用覆盖率；亮度 mask 的覆盖率为 clamp(dot(linear_premultiplied_rgb, [0.2126, 0.7152, 0.0722]), 0, 1)，alpha 已包含在预乘值中。crossfade 对两份冻结输入的线性预乘结果按进度混合，遮罩转场沿用相同覆盖率与组机制。矩形裁剪可优化为 scissor，变换/path clip 使用真正的几何覆盖或 stencil，并保持跨片段、外部内容和离屏组的继承关系。

CPU 实色、文字与图像转换复用 Peniko 导出的 color 类型，按输入的 sRGB/linear 和 straight/premultiplied 标记转换为统一线性预乘值；独立公式仅作为测试 oracle。虚线由 Kurbo 处理偏移、奇数项模式、曲线长度和闭合接缝，再流式交给 Lyon 细分，不维护另一套虚线算法或完整中间路径。全零虚线模式按实线处理。Bevel/Round 不受无关 miter 参数限制；Miter 的非负小于 1 限制等价于 Bevel，不因底层断言拒绝合法帧。网格仍按实际 transient 预算与设备表示能力计量；Kurbo/Lyon 内部临时存储另属依赖工作空间。

GPU clip/group/mask 执行使用按实际存储计账的显式工作栈，不设置 32 或 64 层嵌套门槛；目标、资源与临时存储继续归现有真实预算管理。

设备建立时确定并公开精度、采样数与效果能力。桌面质量档以 RGBA16Float 中间目标和 4x MSAA 为首选；受限档显式使用线性 RGBA8 与 1x 采样，字形覆盖继续保留。具体选择由设备能力与已配置内存档共同决定，创建后保持稳定；变更质量档触发缓存/目标版本更新。桌面离屏起始池为 128 MiB，与普通资源驻留分别限制并汇总峰值。1080p RGBA16Float 的 4x 附件加 resolve 已占 82,944,000 字节，深度、mask 与在途重叠另计；64 MiB 等更小配置在设备/尺寸协商时选择明确的受限档，或报告所请求质量的最小资源量。目标池按实际格式、sample count、resolve、深度/覆盖和同时在途数量预先计量，空间不足时保持既有画面并报告资源需求。质量和内存默认值最终以同一基础场景的实际目标设备数据确认。

Vello 0.8 的编码/渐变与 blend 路径采用 sRGB，imaging_vello 0.0.1 对 alpha mask/filter 另有缺失；因此它不是本合同的默认生产光栅器。CPU replay 可作结构与几何参考，线性颜色期望另用明确公式验证。

设备适配实施合同见 [R02-INTERFACE](../crates/reel-render/R02-INTERFACE.md)；该文档区分 device generation 与 SurfaceEpoch、实际队列完成和表面接受，以及独立目标/资源/临时预算。实现与设备验证状态以 R02 报告为准。

WGPU 适配器使用锁定的 wgpu 30.0.1。设备丢失保留后端提供的 reason/message；已丢失设备上的帧获取、丢弃与呈现统一进入设备恢复，不将清理失败变成进程 panic。帧准备先于交换链获取，提交后仍检查设备错误，只有成功呈现才接受输入几何。恢复只替换 GPU、表面与其缓存，保留剧情 VM 和 CPU 资源；旧设备的 prepared 不能提交到新设备。

### 实施验收

帧、资源租约、可恢复导出和外部内容顺序沿本章合同实现。wgpu 后端在同一合同上验证实际像素、alpha/亮度 mask、嵌套透明组、crossfade、字形与图片缓存、resize/挂起/设备丢失以及上传/回收边界。每份结果区分源码审查、CPU/null 和具体图形设备；完整帧、导出、光栅、合成、提交与物理完成分开计时。

R01 实施接口与边界见 [R01-INTERFACE](../crates/reel-ui/R01-INTERFACE.md)，真实控件、U02 回归与导出/驻留/清理测量见 [R01-REPORT](https://github.com/jhq223/reel/blob/355571b18bd4024914438a7ac6bd94ebf3c7826f/crates/reel-ui/R01-REPORT.md)。自有图片/字体以 core OwnedBytes 将全 capacity 票据绑定到 Blob 底层，生产者缓存别名继续保留该账目；公开 API 导入的不透明源分配仍归生产者单独计量。输出、映射与登记表有硬额度，源 Scene 私有容量不由 command 长度推算。逐元素复制、映射和容量信用可跨 step 推进；实际容量分配与源 Drop 仍是原子操作，分别报告耗时。已知几何外延包含按冻结 DPI 计算的 AA；有限值运算溢出、本地 mask 的采样外延、Gaussian cutoff、字形轮廓与滤镜未知外延明确记为 Unbounded 并要求后端按真实输出目标裁限。记录型 PresentAccepted 是模拟接口结果，null 明确 Skipped，均不代表设备像素或 GPU 性能通过。

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
[5] Masonry 绘制与分层 https://github.com/linebender/xilem/blob/271a27a6d4a930f7878d404f9014e3c50a3a9b88/masonry_core/src/passes/paint.rs
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
