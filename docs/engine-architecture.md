# Reel 引擎架构设计
设计版本 1.0 · 已提交独立审查，接口实现状态见正文。

[文档目录](README.md) · [作者指南 中文](guide/zh-CN.md) · [Creator guide English](guide/en.md)

## 目标与文档边界
Reel 面向视觉小说，以统一舞台组合剧情画面、UI、文字和效果，同一组节点可组成对话框、图片按钮、菜单和演出，再封装为作者组件。引擎复用 Masonry 的控件能力和 Kira 的音频能力，配套语言 Aside 以 Quirrel 为参照，用 Rust 实现编译器和 VM。
本文规定运行时所有权、输入与渲染、媒体、持久化和开发顺序。《VM 与语言设计》负责语法、值模型、执行帧、回收和语言兼容；《游戏作者 DSL 设计》负责作者接口、组件组合与示例。跨文档的等待、绑定和恢复语义必须一致。三份文档交叉审查后成为 reel 仓库 docs/ 下的设计基线。预算与性能数字均为验收目标，尚未形成实测结论。

## 一 产品范围与实现原则
剧情按阅读顺序运行，在对话和选择处等待；角色身份、历史记录与显示组件分开。画面和 UI 共用节点、资源、布局、变换、裁剪和效果组。长期存在的节点通过属性更新，避免每次输入重建整个界面。参考 Ren’Py 的交互与屏幕复用经验。[1][2][3]
核心提供存档、设置、历史、对话与选择的数据模型及操作接口。标题页、设置页和存档页由作者组合；项目脚手架只生成配置和入口，不附带一套完整内置 UI。用针对性契约用例验证这些能力，作者文档中的片段用于说明组合方式。
初期使用一个工作区，先落地默认 winit 窗口适配和 wgpu 渲染器，窗口管理与渲染后端可以独立选择。视频、软件渲染、静态子树纹理缓存与增量快照按实际需求推进。现有 ons-rs、krkr-rs 用作结构参考，不移植旧引擎特定命令。[4]

## 二 模块职责与依赖
工作区按职责划分，语言包可独立发布。
- aside-vm：语言值、对象、调用帧、模块与执行接口。
- aside-compiler：解析、分析、字节码及源码定位，依赖 VM 公共格式。
- reel-stage：逻辑节点、属性、事务、事件映射与 Masonry 适配。
- reel-render：后端接口、资源上传、缓存与分组合成，读取 stage 提交结果。
- reel-media：Kira 适配和播放控制，视频作为可选模块。
- reel-engine：会话、剧情任务、宿主绑定、资源与持久化协调。
- reel-player：平台适配、窗口与事件循环，组装引擎、渲染和媒体输出。
- reel-cli：检查、开发运行和构建，调用编译器与播放器库入口。
语言包不依赖窗口、舞台或媒体。业务数据类型只在所属模块定义一次，跨线程传递拥有自身数据的消息或线程安全只读缓冲，不传 VM 堆引用。渲染器通过资源 ID 与版本取得内容。
通用依赖直接使用锁定版本，不复制进仓库。Parley、winit、wgpu、Kira、image、serde、atomic-write-file 与 codespan-reporting 暂按普通依赖管理；只有确认必须修改上游代码时，才评估纳入源码。Masonry 先验证适配层能否满足需求，再决定是否维护仓库内分支，不预先复制整套 UI 生态。
third_party/quirrel 保存 Quirrel 4.41 的固定源码与对应测试快照，基线为 19f17df9b6ee43c91ef5c635fd45970ec76c5dbe，用于规范核对与差分测试。它属于开发工具，不链接进发行播放器；Aside 编译器与 VM 是工作区自己的 Rust 实现。[6]
Masonry 既定核对基线为 271a27a6d4a930f7878d404f9014e3c50a3a9b88。嵌套效果与分层输出仍需验证，不能据此推定现成支持。[5] 若必须修改，将所需上游 crates 放入 third_party/masonry，同时保留必需的同级路径依赖、工作区继承配置和构建资源。通过工作区 path 依赖接入，统一同名库的来源并检查依赖树，避免意外同时链接仓库内版本与上游版本。
仓库内第三方源码采用固定快照，不递归保留嵌套 .git。每份快照记录上游地址、提交、版本、导入日期与本地修改，保留并核对许可证及版权声明；更新时可追溯差异。当前仅 Quirrel 对照快照和可能需要修改的 Masonry 属于候选范围，其他依赖没有已确认的复制需要。
播放器的 source 能力选择性引入编译器；精简版本只加载字节码。hot-reload 只开启模块替换协议，可接收外部编译产物。编译器、监视器和对照运行时留在开发侧，精简构建检查实际依赖树。[7]

## 三 平台接口与移植边界
窗口和渲染分别选择，默认组合为 WinitHost 与 WgpuBackend。reel-player/platform 定义 PlatformHost，reel-render 定义 RenderBackend，reel-media 定义 AudioOutput；AssetIo、Clock、WorkExecutor 在消费它们的 reel-engine 服务接口中定义。player 组装适配器并转换输入，engine 与 stage 不反向依赖 player。Masonry 控件与布局适配同默认窗口外壳分开；这是需要验证的隔离目标，不能假定上游已经完全平台中立。
原生窗口或固定显示目标的创建、表面来源协商与销毁留在具体 player 组装代码，不进入公共 trait。可借用或包装原始句柄，但有效期必须覆盖表面使用期；先停止提交并释放表面，再销毁显示目标。winit 事件、wgpu 设备与纹理、Vita 原生句柄都不得进入会话、RenderFrame 或 DSL。每个后端负责上传、格式转换、离屏目标、设备重建和完成后回收，不能仅替换最终绘制函数。
Cargo features 决定编入哪些适配器，运行参数只在已编入且能力匹配的组合中选择。游戏包声明必需能力；缺失时报错，有声明的低质量后备才可降级。Masonry 与默认外壳、Kira 与默认输出的目标依赖都要检查，关闭桌面 feature 后不能仍被传递依赖强制引入。不建设动态插件系统或通用平台框架。
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
    fn stop(&mut self) -> Result<(), AudioError>;
}
pub trait AssetIo {
    fn capabilities(&self) -> IoCaps;
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
- 身份：各类请求 ID、FrameId、表面代数使用相互独立的 u64 包装类型。Scope 为 Session(epoch, owner_id) 或 Process；保存写盘使用 Process。MonoTime 是单调纳秒 u64，UnixMillis 是可缺失的 UTC 毫秒 i64，均不含本机指针。
- Rejected<T> 保存原始 request:T 与 reason。reason 至少区分 Busy、Unsupported、InvalidRequest、Unavailable、OutOfMemory 和服务故障，附拥有型诊断文字；只有 Busy/Unavailable 等明确可重试原因允许原请求重试。拒绝不产生已接受 ID 或业务副作用。已接受请求的终态为 Completed(payload)、Cancelled、Failed(error)，结果记录包含请求 ID 与 Scope。Cancelled 用于释放与记账，不重新唤醒已取消业务。
- QueueLimits 含 max_requests:u32、max_bytes:u64；BudgetTicket 含预算类别和 bytes:u64，随拥有的数据转移，最终释放一次。接受请求时同时保障完成结果的队列容量。WorkBudget 含 max_units:u32、max_bytes:u64、deadline:Option<MonoTime>；WorkProgress 报告实际消耗与是否仍有工作。
- PlatformCaps 含指针、触摸、手柄、文本输入、剪贴板、可访问性、挂起恢复、等待及唤醒支持位。TextInputState 含启用值和逻辑坐标的插入点矩形。输入事件带输入序号、设备身份和事件时间；尺寸事件同时携带物理像素、逻辑尺寸和缩放系数。
- SurfaceInfo 含 physical_size:[u32;2]、scale_factor、surface_epoch、color_encoding 与 alpha_mode；零尺寸代表暂不能呈现。RenderCaps 含支持格式/效果集合、纹理与离屏尺寸上限和上传队列限制。UploadBatch 拥有资源版本、尺寸/stride/格式、像素或网格数据及预算票据；所有乘法和长度校验先于接受。
- PresentOutcome 为 PresentAccepted(FrameId)、Skipped(reason)、SurfaceUnavailable(reason)。RenderEvent 至少有 UploadFinished(id,result)、ResourcesRetired(versions) 和 DeviceLost(error)；可选的实际呈现反馈另带 FrameId 与时间，不能替代资源回收通知。返回的 FrameId 必须对应本次提交帧。
- ReadRequest 含资源身份与版本、offset:u64、len:u32、优先级、Scope 和预算票据；读取结果拥有 bytes，并明确 eof，长度不得超过请求。SaveWrite 含槽位、提交序号、完整槽位数据、最低持久提交级别与预算票据。IoCaps 声明范围读取、读取/保存队列限制及可提供的提交级别；只保证可见替换、不保证持久性的实现不得报告持久保存成功。
- WorkRequest 含 kind、拥有型输入、Scope、优先级及预算票据；kind 限于已注册的解码、编码、校验操作。WorkCaps 含 Threaded/Cooperative、支持种类和队列限制；不支持的 kind 在接受前拒绝。处理进度不算终态。
## 平台与时间
PlatformHost 提供命令，原生事件循环驱动 Player::on_event，不要求平台提供反复阻塞的 pump_events。PlatformEvent 包括输入、组合文本、焦点、尺寸与 DPI、挂起、恢复、内存压力和关闭；输入采用引擎自己的键盘、指针、触摸和手柄数据。剪贴板请求拥有读写参数，以 ClipboardId 关联一次结果；可访问性更新使用稳定节点身份，平台动作从同一事件桥接返回。不支持的能力明确拒绝。后台只持有窄 WakeSignal 唤醒循环，不借用整个 PlatformHost。
PlatformCaps 声明输入、文本服务、可访问性及等待/唤醒能力；重绘可合并。MonoTime 使用固定单位的 u64，不保存 std::time::Instant。会话逻辑时钟独立累计，可暂停与恢复；墙钟可缺失，只用于显示日期等。恢复后重设截止时间，系统睡眠不得变成巨大动画增量。
## 渲染与音频
SurfaceInfo 只含尺寸、缩放和引擎颜色约定。RenderCaps 声明纹理/离屏上限、格式和效果支持集；RenderFrame 保留画序、分组、裁剪与颜色/alpha 语义，具体路径和字形转为可适配的几何、掩码或图集。效果以 ID 和纯参数表达，只有 WGSL 实现的效果不能宣称可跨后端使用。
render_present 只在调用内借用帧；后端若保留命令，必须取得数据和资源引用所有权。上传批次包含资源版本与预算票据；接受后由后端持有直到完成或失败。retire 只禁止新引用，实际 GPU 使用结束后才回收。表面不可用或设备丢失须报告并重建，保留会话状态；不能继续使用旧 GPU 身份。
呈现结果区分 PresentAccepted、Skipped、SurfaceUnavailable。PresentAccepted 表示交给呈现系统，不证明扫描显示或 GPU 使用完成。PresentAccepted 更新命中快照；提交成功、实际显示与 GPU 回收分别记录。平台可额外提供实际呈现时间。
AudioRequest 含首选采样率、允许的设备格式/布局与最大供给帧数。AudioCaps 描述实际可协商配置、供给块范围及挂起能力，不将采样率与布局的任意组合都视作有效。configure 只在停止状态调用，返回 AudioFormat：实际 sample_rate_hz、固定 mixer_channels=2、device_sample_format、有序 device_channel_layout、max_pull_frames 与 channel_map。规范混音格式固定为交错 f32 的 L、R 双声道，采样率采用协商后的设备采样率；设备的 8 声道不意味着要求混音器输出 8 声道。Kira Frame 的左右声道与此对应，媒体层在协商后初始化其 Backend 桥接。[14][15]
输出适配器负责从规范混音到设备布局与格式的转换。单声道为 0.5×(L+R)，立体声映射 L/R；多声道默认仅映射到已识别的前左/前右，中心、低频与环绕填零，不擅自扩展声场。未知声道布局必须重新协商已知布局或返回 Unsupported。Windows 8 声道用例核对实际通道顺序、采样率与缓冲长度，不能只检查设备通道数量。
每次 PcmFill 接收 n 帧、恰好 2×n 个 f32，1≤n≤max_pull_frames，切片只在调用期间有效。设备块超过上限时分块供给；设备缓冲长度必须为设备声道数的整数倍。输出适配器预分配转换缓冲，每次先清零供给区，混音器覆盖完整样本；异常长度不得越界，须静音并报告设备契约错误。非有限样本替换为零，转换为整数格式时限幅。回调路径不分配、不读取文件、不等待主线程。
PcmFill 仅拥有混音状态、预分配缓冲与有界控制队列，不捕获 VM 或舞台。start 在接受前完成设备准备；拒绝返回尚未调用过的原 fill 及原因，可在兼容格式下重试；格式变化必须重建 fill。接受后供给由唯一输出线程串行调用，后续设备失败只通过 AudioStatus 报告。AudioStatus 含 Stopped/Running/Suspended/Failed、欠载计数和最近设备错误。stop 无论返回成功或错误，都须先保证不再调用 fill；不得在 fill 内等待 stop。欠载填静音，断连与采样率变化先停止，再配置并重建混音供给；不在后台悄悄更改格式。
## 资源与工作执行
ReadRequest 使用 AssetKey、版本、offset:u64、受限 len:u32 和优先级，不要求路径、mmap 或文件监视器。AssetIo 可读包、内存或平台文件系统，接口须快速返回；真正可能阻塞的文件调用交给平台异步机制或 IO 工作者。SaveWrite 使用应用保存命名空间和独立进程请求身份；完成只表示该平台已验证的持久提交级别。缺少可靠原子替换的平台须采用经故障验证的双槽或版本日志，不能假定 rename/fsync 一定适用。
WorkRequest 是图像解码、快照编码、包验证等有限工作描述，拥有输入、优先级、取消身份与预算票据，不接收脚本闭包。WorkCaps 声明线程或协作模式及支持种类。协作实现保留游标，在工作单位、字节或截止时间到达时让出；不能用一次阻塞整图解码冒充分片执行。无工作线程且无可分片实现时，明确不支持该工作或要求预转换资源。实时音频供给不能被普通协作任务饿死。
所有队列同时限制数量和字节。拒绝返回原请求及原因，不丢数据、不暗中改为同步执行。完成消息拥有自身数据，包含请求身份、所有者或会话代数，最多一个终态；取消即使不能中断底层工作，也不得再交付业务结果，最终释放额度。poll/drain 每次最多写入 limit 个事件，调用方预分配容量。IO 读取与保存各自保留容量。
## 桌面与 PS Vita 目标
首批桌面目标是 Windows、Linux、macOS，各组合完成构建和行为验证后才列为支持。移动端与 Web 需要另外适配生命周期、输入、媒体与存储。PS Vita 是未来移植目标，当前接口不代表已经可运行。
Rust 的 armv7-sony-vita-newlibeabihf 目标为 Tier 3，依赖 VitaSDK，标准库部分支持且无预编译标准库；VitaSDK 是开源 homebrew 工具链。[10] 因此先验证锁定工具链和完整依赖树，不从 target 存在推导 Masonry、Parley、Kira 等已兼容。
Vita 的 GXM 有专门纹理布局、内存映射与同步要求，GPU 预算需按不同内存区域实测。[11][12] 音频提交可能阻塞，应由专用路径处理，输出适配器按实际端口把 f32 转换为所需 PCM，转换缓冲预分配。[13] 32 位构建不保存指针或 usize 布局；长度转换、stride×height 与对象索引做溢出检查，不假定 AtomicU64 可用。桌面 N100 的帧时间和资源预算不直接用于 Vita。

## 四 线程与帧调度
主线程拥有窗口、舞台、Masonry 和 GPU 提交。VM 在主线程按预算执行，避免细粒度跨线程调用。默认桌面把读取、解码和存档编码交给工作线程，其他平台依所选 IO 与执行器契约推进；音频由独立输出路径保障。后台队列同时限制任务数和数据字节数，当前可见资源优先于预取。
每帧固定执行：输入分发；预算内脚本与后台完成消息；动画采样；状态提交；必要布局与排版；命中几何与绘制计划更新；呈现。布局属性动画参加本帧布局，纯变换与透明度只影响合成和命中。静止画面等待输入、计时器或资源事件，活动动画才持续请求新帧。
输入采用最后一次被呈现系统接受的帧几何，收到 PresentAccepted 才替换快照。单次输入固定使用同一份快照；获取表面、提交或呈现失败、过期，以及 Skipped 或表面不可用时都保留原快照。该规则不承诺屏幕已经扫描显示。命中后核验当前节点代际与交互有效性，已销毁节点不再接收事件。一次输入最多推进一次剧情。
Masonry 适配层是唯一交互分发入口。控件先处理并消费输入，剩余推进事件才进入剧情。按钮配对按下和释放，滑条捕获拖动指针。模态容器隔离下方指针、焦点、键盘和剧情快捷键，并释放作用域外的捕获与快进保持；失焦清除捕获与按键保持。

## 五 统一舞台与合成
StageGraph 唯一拥有作者可见节点、稳定 NodeId、父子结构、声明属性与寿命。Masonry 负责对应控件的布局、焦点、捕获、输入法、可访问性和临时交互状态；复合控件内部节点保持私有。两者通过 NodeId 到 WidgetId 映射连接。业务值和控件瞬态不得成为两份互相覆盖的状态。
脚本变更与控件动作进入有序 StageTransaction，支持创建、移除、挂载、脱离、替换场景、属性修改、绑定和订阅。提交前统一验证身份、单父与无环约束；失败保留原舞台，成功后才释放旧场景。属性存储是唯一权威，同一事务同一属性采用最后一次有效写入，范围约束先执行，随后静默更新投影。属性绑定保留被引用属性的读写权限，slider 写回需要可写属性；只读派生绑定跟踪实际读取的可观察属性；派生函数不得写入或等待，循环依赖给出源码诊断。
节点内容与交互行为分开。图片可以具备按钮行为，容器可以整体变换；按钮外观读取 hover、pressed、focused、disabled 等状态，滑条提供轨道与滑块槽位。作者组件与内建组件使用同一舞台能力，具体调用形式见 DSL 文档。节点移除注销绑定、订阅并取消所属任务与活动；隐藏保留身份，脱离必须由显式所有者接管。visible=false 隐藏子树并退出命中与焦点；enabled=false 保留画面并禁用子树交互。两者使捕获失效时统一释放捕获；opacity=0 不自动禁用输入。
仿射变换、裁剪和命中使用同一几何，命中经逆变换计算。非线性视觉变形默认采用作者指定的逻辑命中区，扩展映射另行提供。适配必须验证变换后控件的捕获、焦点与命中一致性。
RenderFrame 包含帧号、画布与输出尺寸、资源引用以及有层次的绘制计划。计划保留 BeginGroup、EndGroup、裁剪、组透明度和效果参数。组内绘制连续，子节点排序受组边界约束；跨组覆盖由显式 overlay 容器表达。组透明度对合成结果应用一次，效果外扩计入离屏范围，再遵守对应裁剪语义。
普通容器直接合成，遮罩与后处理按需使用离屏目标。先实现相邻兼容绘制合批、排版与字形缓存、离屏目标复用；逐字显示复用完整文本排版，只改变显现范围。属性变化区分布局、内容、合成和命中；淡化不使排版失效，资源版本变化传播到依赖缓存。GPU 资源在实际使用结束后释放。低端模式可降低效果分辨率和采样数，逻辑坐标保持一致。

## 六 VM 与会话接口
会话管理主剧情、UI 回调任务、交互数据和活动所有权。主剧情同一时刻只有一个交互。game 服务身份稳定，game.dialogue 属于当前会话，settings.music 属于全局设置。发言更新完整文本与显现范围并追加一次历史，再等待推进；文字未显示完时首次推进只完成文字，后续推进才结束等待。选择返回稳定业务 key。入口与剧情 helper 均为普通 fn，剧情元数据按需附加；具体语法见语言与 DSL 文档。
VM 的 run_slice 接收指令、分配和计时检查额度，返回完成、预算让出、宿主调用、等待或错误。宿主调用携带任务身份、参数和源码位置，返回 Ready、Pending 或 ScriptError。Pending 先退出 Rust 调用，再建立显式恢复记录；恢复记录保留操作、可恢复参数、返回目的槽与下一指令位置。禁止把原生调用栈当作可保存续体。同步原生重入 VM 不得跨边界挂起，异步宿主算法使用显式状态机。
SessionEpoch 在新游戏、读档和回滚时更新；任务、等待、节点、回调和活动各用独立代际身份。后台完成结果只有与当前会话及等待匹配时才交付一次。取消、节点销毁和会话替换使旧结果失效。
ActivityHandle 支持暂停、继续、停止和完成等待。终态为 Completed、Stopped、Cancelled、Failed，只提交一次；前两者返回相应结果，后两者在等待位置产生取消或脚本错误。所有者取消任务时直接终止该等待。所有 VM 任务遵循同一可等待对象契约，恢复身份进入快照。主剧情输入权限仍由会话管理，与函数声明形式分离。
演出描述先纯构造，再由 run 在显式所有者下启动；顺序组合不接收已运行句柄。动作激活时读取起值。run 先检查同一时刻的属性写入冲突，冲突则保持已有活动并返回源码错误；作者先停止旧活动再接管。显式属性赋值停止正在驱动该属性的整个 tween，再采用赋值。子动作 Stopped 使 sequence 停止后续项、parallel 停止同组余项，整个组合返回 Stopped；子动作 Failed 则按相同范围结束并返回错误。持久活动图包含子活动、进度与等待关系，并提供可序列化恢复描述。描述中不保存闭包，事件回调排入普通任务。接口见 DSL 文档。
已读标记按稳定语句 ID 记录。按住快进由输入保持状态驱动，松开、失焦、进入菜单和读档时清除；自动播放属于会话状态，文字速度和音量属于全局设置。更换 UI 不改变历史。

## 七 资源与音频
资源以稳定 AssetKey 和内容版本管理读取、解码、上传与缓存。取消预取、场景退出与过期结果都释放所属额度。图片、字形、视频队列、GPU 纹理和临时目标分别记账；缓存只保留可逐出的额外引用。直接资源引用与作者预加载共同提供预取提示，动态分支确定后调整集合。
默认音频用 Kira 适配音乐、语音、音效与 UI 分组；各目标输出桥接单独验证。脚本发送控制，实时供给不得执行文件读取和 VM 工作。控制队列有界，连续音量更新可以合并；播放补给拥有独立工作容量与队列额度，图像解码不得耗尽它。短音效预解码，长音轨采用流式策略。[9]
预分配回调路径、锁竞争、声道与采样率协商必须实测。Windows 8 声道设备配置列为重点用例。控制队列为停止、设备关闭与音轨释放预留独立容量；播放请求进入普通有界队列，满载时返回带原请求的 Busy。音量按音轨合并为最新值。停止请求按音轨幂等合并，关闭状态优先于新播放，保证回调线程始终有界且主线程获得明确结果。具体容量由目标配置给出，满载场景进入契约验证。
视频暂为可选能力，具体播放管线、插件与发行方式另做小范围验证；能力缺失在项目检查或包加载时报告。

## 八 存档与回滚
存档在所有可修改持久状态的任务停在合法指令或等待边界，且舞台、绑定和资源切换已提交后捕获。保存、读档提交与热重载提交互斥。保存接口立即返回请求，进度由服务模型提供；发起保存的 UI 回调先结束，避免读档后重新执行保存按钮。
快照包含引用模块版本、全部存活任务及其调用帧与恢复位置、返回槽、异常状态、寄存器与中间值、闭包和共享 cell、全局对象图、待交付结果、当前交互、组件数据、舞台逻辑状态、时钟、随机数及媒体描述。generator、thread 与其他可等待对象按语言定义保存。对象 ID 保留共享、环、弱引用和类关系；脚本回调及捕获进入同一对象图。
设备句柄通过资源描述重建。每种可达宿主值必须有版本化恢复描述；缺失时报告对象路径，保存失败且保留原槽位。系统服务恢复为同一服务身份。读取在隔离候选会话中先创建身份、再连接引用与订阅，成功后一次切换；失败保留当前会话。
捕获先固定会话时钟、根与媒体逻辑状态，再暂停业务写入，按对象与边游标分片复制到独立快照。长字符串与容器也应分块。窗口、输入采集和控件瞬态反馈继续运行；业务回调和后台结果排队，完成捕获后再处理。连续移动按指针合并，按下、释放、取消保留次序。队列接近容量时提前终止本次捕获并恢复业务处理，保存请求报告 CaptureBusy，原存档保留；仍发生输入溢出则发布 InputReset，释放全部捕获与按住状态并丢弃该溢出批次，避免卡住按钮或快进。队列内事件保留会话和节点身份，恢复后重新验证。后台只接收已完成的纯数据快照，负责压缩与写盘。
资源读取按描述重发，动画按逻辑时间继续，音乐按资源和位置恢复，当前语音从该句开头重播，一次性音效保持已结束。外部写入使用结果记录与回滚边界，不能随读档盲目重放。
槽位文件统一保存摘要、缩略图和完整快照，桌面默认在必要同步后原子替换，其他平台遵守 AssetIo 已验证的等价持久提交策略；同槽请求按序提交，只有磁盘提交成功才报告完成。失败保留旧文件。捕获后的写盘由进程级保存服务持有，切场景不取消它。平台同步与替换行为必须实际验证。自动保存可合并，显式请求保留顺序；待捕获请求数量与估算字节数有上限。
全局设置、已读和解锁记录独立于剧情快照。恢复设置绑定时以当前全局设置为准。回滚先使用完整交互快照和共享不可变资源，默认历史预算起点为 32 MiB，超限移除最旧快照；活动 cell、属性和等待状态必须独立复制。增量记录等真实成本明确后再做。
每个闭包和挂起调用绑定确定代码版本；旧模块保留自己的全局环境与导入绑定，最后引用退出才释放。发行存档由匹配游戏包提供代码；开发存档嵌入无法从匹配固定包取得的全部所需模块，包括活动版本。缺失版本进入明确迁移流程，不自动改用新代码。

## 九 热重载
开发工具合并变更事件，编译并验证候选模块；失败显示源码诊断，继续运行原版本。新版本只在安全点提交。资源也先准备新版本，再替换引用。[8]
默认保留正在执行的剧情代码，新入口和重建组件采用新发布模块集合。组件依稳定 key 和局部数据重建，替换订阅后释放旧订阅。拖动中更新推迟至手势结束或明确取消，业务状态与控件瞬态分别处理。
切换当前剧情属于显式开发操作，按命名段落与交互点检查状态结构；从段落入口重跑必须提示会重新执行该段动作。普通读档和回滚始终恢复完整快照。候选模块与舞台替换完整准备后统一提交，失败保留旧版本。开发热重载不承担任意旧发行存档的自动迁移。

## 十 性能目标与可观测性
固定 720p、1080p 验证场景记录冷启动、包加载、输入响应、CPU/GPU 帧时间、分配、内存与纹理峰值。首阶段以 Windows x64 作低端体验基准，Linux 与 macOS 各自验证窗口、输入、渲染、音频和持久化。低端参考档为 N100 级集成显卡、8 GiB 内存、720p；每份结果注明实际硬件与构建配置。
目标为控件可见响应 p95 小于 50ms，基础动画 CPU 帧工作 p95 小于 8ms，GPU p95 小于 12ms，呈现间隔 p99 小于 33.3ms。60Hz 稳态同时要求间隔中位数小于 17.5ms、超过 25ms 的比例低于 1%。这些均待实测验证。
主线程起始调度配额为脚本 2ms、后台结果与资源安装 1ms、回收 0.5ms、快照捕获 1ms；VM 至少每 256 个工作单位检查时间，分配与释放另计。单次不可分割操作同样测量，超限不能只靠总额度掩盖。GPU 上传按帧字节预算分批。快照暂停造成的业务排队时间单独记录，小场景目标一帧内完成。
资源预算起点为 VM 堆 32 MiB、CPU 解码驻留 64 MiB、GPU 纹理 128 MiB、离屏工作目标 32 MiB；峰值包括待释放对象、候选模块和快照副本。超限先逐出可选缓存、延后预取，再报告具体资源需求。队列与回收积压纳入背压，不能无限增长。
每阶段必验：静止场景无无效脚本、布局和呈现；单滑条不更新无关布局；纯音量变化只通知音频；单节点淡化复用排版。开发运行和 release 都测体验，优化由同一工作负载的前后数据证明。

## 十一 开发顺序与未决验证
D01 固定公共协议、版本规则与验收样本，验证独立窗口与渲染接口、表面生命周期及能力选择；再验证 Masonry 分组绘制、变换命中和控件外观接入。队列按本文规则实现，D01 固定各目标默认容量并覆盖满载情况。
L01 实现语言最小闭环及宿主调用。S01 并行完成舞台事务、图片按钮、滑条、tab 和模态输入。R01 完成组透明度、裁剪、遮罩与缓存失效。M01 并行完成 Kira、资源取消和预算实测。以上共享 D01。
E01 验证剧情等待、作者组件绑定、历史与快进的会话契约。P01 完成对象图快照、跨进程读档、多槽和有限回滚。H01 在既有状态表示上加入热重载。完整语言兼容按独立清单持续推进，属于最终验收，不能把语言最小闭环称为语言完成。
验收采用最小宿主夹具和针对性 UI 片段，不要求制作完整可玩流程。NullPlatform、记录型渲染器、假时钟和内存 IO 验证无隐式设备、时间或文件系统依赖；关闭 winit/wgpu 后检查核心及传递依赖。用例覆盖单次输入消费、配对按下与释放、拖动失焦、模态隔离、嵌套合成、保存后切场景、读档后旧结果、重载已有回调及节点销毁后的异步完成。音频、恢复与帧预算分别提供可重复的负载和观察值。协作执行器测单步最大耗时、满队列、取消与过期完成；表面丢失及挂起恢复测时钟和资源身份。Vita 承诺前先做 32 位交叉构建，再依次验证启动、输入、图片/字形、音频、可靠保存与效果，不承诺未经实测的帧率。
每个代理任务写明目标、允许目录、冻结的接口版本、已有代码、禁止扩大的范围、行为场景与性能预算。Issue 按可验收成果组织，PR 保持独立可审查；提交包含实际改动、相关用例、运行命令、观察结果与未解决项。不得以测试数量、模拟成功或完成报告代替实际契约验证。接口或依赖变更先提交设计修改，经负责人确认后继续。负责人亲自审输入、所有权、保存、音频和平台关键路径，重跑关键验证并处理跨模块冲突。性能计数器从第一阶段建立，协议变更同步三份设计。视频后端、存档迁移格式及依赖适配成本仍需专门验证，未验证结果不得写成能力承诺。

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
