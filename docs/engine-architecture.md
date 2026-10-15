# Reel 引擎架构设计
设计版本 1.1 · 开发基线，接口实现状态见正文。

[文档目录](README.md) · [作者指南 中文](guide/zh-CN.md) · [Creator guide English](guide/en.md)

## 目标与文档边界
Reel 面向视觉小说，以统一舞台组合剧情画面、UI、文字和效果，同一组节点可组成对话框、图片按钮、菜单和演出，再封装为作者组件。引擎复用 Masonry 的控件能力和 Kira 的音频能力，配套语言 Aside 以 Quirrel 为参照，用 Rust 实现编译器和 VM。
本文规定运行时所有权、输入与渲染、媒体、持久化和开发顺序。《Aside VM 与完整语言规范》负责语法、值模型、执行帧、回收和语言兼容；《Reel 游戏开发指南》负责作者接口、组件组合与示例。跨文档的等待、绑定和恢复语义必须一致。三份文档交叉审查后成为 reel 仓库 docs/ 下的设计基线。预算与性能数字均为验收目标，尚未形成实测结论。

## 一 产品范围与实现原则

Reel 的脚本服务通过独立 [Aside host SDK](aside-host.md) 接入。节点、会话和活动业务留在 Reel；可复用的 Rust native 声明、编译链接、运行时准入和属性 schema 生成归 Aside 扩展层。绑定宏从同一声明产生元数据与调用包装，不复制舞台类型或引入第二套语言实现。

脚本堆的对象、宿主根和根区间存储按实际需求增长，默认不以固定数量或字节额度限制作品规模；宿主可以显式设置资源配额。GC 与释放队列在对象准入前准备容量，增长保留代际身份与扫描状态；队列、根映射及增长时的新旧存储都进入同一内存计账。执行任务和切片预算独立于这些堆存储额度。

组合动作的宿主声明先接通普通可变参数：NativeContract 的 rest 名称和 Rust `#[aside(rest)]` 映射为既有 B01 array rest。sequence/parallel 等库接口使用这条通路，不新增 Reel 专用编译语法或另一套参数收集器。范围和验收见 Aside host SDK 文档；动作组合运行器共用下面的活动合同。

作者播放器只呈现显式挂载的舞台，不自动创建对白/选择控件或复制内置视图投影。game.configure 移除默认界面开关，native ABI 为 3；game.dialogue()/game.choice() 保持数据与交互服务。Rust 会话诊断的预览由诊断入口显式创建，不参与 Aside 作品设置。

### 作者活动与窗口时钟

`game.exit()` 请求结束当前作品，不调用进程退出或持有平台句柄。请求一经接收便不再执行脚本；宿主沿既有关闭路径取消任务、活动和资源，增量完成清理后关闭作品窗口。嵌入宿主可读取 SessionVm::exit_requested，并决定其外层应用如何处理。新增 native ABI 1，B01/ASB 不变；范围为 reel-engine、reel-player 与双语指南，无新增依赖，退出标记为常数空间。验证主剧情和 UI 回调退出、未决等待清理及实际窗口关闭。

普通 UI 流程使用 `completion(owner: null)` 创建一次性结果，`await result` 等待，回调用 `result.resolve(value: null)`、`result.reject(error)` 或 `result.cancel()` 结算。owner 省略时沿用活动规则：主剧情归会话，回调归事件节点；显式节点退休使未结算结果取消。第一次结算返回 true，之后返回 false；null 也是合法结果。重复等待读取同一终态，resolve 原样返回值，不采用另一个可等待对象。结果不占用对白/选择权限，不记录剧情历史；组件可以返回 `{root, result}` 供主剧情或其他回调组合。

此接口属于 Reel 的宿主 Awaitable，复用 Aside NativeFrame、WaitId、错误传播与任务取消；新增 native ABI 1，B01/ASB 不变，不增加依赖。结果、owner 和任意返回值在 VM 堆中追踪，终态释放 owner；服务只保存尚未交付的等待订阅，取消等待者不取消结果。订阅按登记顺序分批维护，每步最多检查 64 项，一轮完成后才调度脚本，避免结算顺序随切片变化。空闲不启动计时轮询，未被引用且未被等待的结果可直接 GC。验证范围为 reel-engine、公开组件示例、双语指南与本地移植烟测，覆盖源码/ASB、表单及子游戏返回值、失败、节点退休、多个消费者和 GC。

工程直接从 main.aside 启动。main 中的 `game.configure(title: null, width: null, height: null, fonts: null, text_speed: null)` 更新会话的呈现设置；省略项保留当前设置，参数按普通 Aside 表达式求值。设置以不可变值发布给宿主，宿主只在值身份变化时应用。画布采用 contain 变换并共用绘制/输入几何；未设置画布时沿用视口。设置首个画布维度时，另一维默认 1920×1080 的对应值。fonts 接受一个字体文件或非空有序数组；作品须在首次呈现前显式配置字体，缺少字体时报错。节点 font 使用已提供字体中的字体族/回退列表，system-ui 指向配置的默认列表。文件相对资源根解析并共享计量后的数据，字体回退和文字整形复用 Fontique/Parley；作者播放器关闭系统字体发现。平台窗口句柄始终留在适配器。ASB 1.0 元数据 schema 3 只保存代码图与入口，运行设置由脚本执行产生。

`sequence(...actions)`、`parallel(...actions)` 与 `repeat(action, times: null)` 产生不可变、可复用的描述。空组合与零次重复无写入地立即完成；持续循环要求单轮时长大于零，包含无限分支的组合在 finish 修改状态前报错。run 在接管前完整校验并行分支的写集合，顺序段到激活时才读取起值与取得属性。暂停、停止、失败、目标或 owner 退休均按整个根活动结算。

每次 run 在 VM 堆中构造可追踪的执行图；同一描述的多个出现位置拥有独立进度，重复次数不展开成节点。构造、公共祖先冲突检查、时间推进、控制和终态清理使用显式游标，按图节点/祖先计工作额度，不递归占用宿主栈。运行时按截止时间处理跨段事件，同一时刻先结束已有段再激活后续段；循环复用起值、版本和计数槽，不分配新 VM 载荷。时间轮结束前保留上一完整舞台投影，脚本任务边界也触发维护，以处理暂停期间的赋值与退休。终态清除描述、目标和执行图强引用；等待者取消不取消生产者。演出示例见 [author_activities.aside](../examples/author_activities.aside)。

本次范围为 aside-vm 的宿主属性写入追踪、reel-engine 活动服务、reel-player 调度、示例及双语指南；保持 B01/ASB v1、Reel native ABI v1，无新增依赖。有限动画使用共同节点数值属性，`tween(node, x: ..., duration: ..., easing: "linear")` 描述动作，`run(action, owner: node)` 启动；owner 省略时，主剧情归会话、按钮回调归事件节点。`pause(activity)`、`resume(activity)`、`stop(activity)`、`finish(activity)` 控制同一活动；await 返回字符串 `Completed` 或 `Stopped`，目标或 owner 退休产生 Cancelled。

节点目前共享固定数值 schema，因此 tween 从同一节点属性描述生成普通命名参数，不为此引入第二套动态参数绑定器；未知属性由现有编译器/VM 拒绝。PropertyBagTail 仍是自定义 schema 的后续语言能力。null 表示该属性不参与动画；width/height 的起值必须已显式指定。duration 为有限非负秒数，零时长立即提交终值。easing 支持 linear、in、out、in_out 的线性或三次曲线；起值在启动时读取，动作描述可复用。

活动的写入所有权调整为接管语义：新活动在真正启动属性段时停止与它冲突的旧根活动，之后读取当前起值。暂停保持属性占用，但不阻止新演出接管。直接赋值（包括赋相同的值）停止写该属性的根活动；其他属性的赋值不影响它。退休、取消、停止、完成均只结算一次；等待者取消不取消生产者。终态保存在脚本持有的活动对象中，活跃根及订阅随结束释放，不积累历史活动索引。

库宿主通过显式单调逻辑秒数推进活动；时间片计时与剧情时间分离。推进按调用者给定的活动工作额度分片，同一轮固定采样时间，未完成时保留上一完整呈现；属性占用直接索引对应活动，启动接管和终态摘链不扫描历史活动。窗口只在有运行中的动画时请求后续时间更新，暂停和纯输入等待可休眠。动画直接写现有节点字段，变换沿现有 compose 路径，不逐帧分配脚本对象或重做文字布局。验收检查源码/ASB、终态/取消/接管、时钟和画面投影，记录稳定推进期间堆分配与布局次数；所有动作、活动、属性写入版本与占用关系都位于既有 VM 堆；单一活动链根保活生产者，不设活动数量上限。会话适配器已调度主任务与按钮回调，活动等待维护多个独立消费者；取消消费者不取消共享生产者，订阅随真实等待失效或交付释放。可运行示例为 [author_activities.aside](../examples/author_activities.aside) 和 [author_buttons.aside](../examples/author_buttons.aside)。sequence/parallel/repeat 已贯通同一活动协议，scope 与快照恢复仍未实现。
作者能够以符合直觉的接口表达、组合和改变想象中的画面与交互，是实现和设计取舍的共同标准。可运行的小场景用于验证表达能力，不限定引擎最终范围。实现过程中发现不自然的接口，应同步改进规范和作者指南，不为维持尚未发布的内部接口引入兼容层。

`delay(seconds)` 描述有限非负时长的等待，与 tween 共用 run、sequence、parallel、repeat、暂停、停止、完成和 owner 取消协议；等待不拥有舞台属性，也不需要虚拟节点。`await run(delay(1))` 等待一秒，零时长立即完成。纯等待按最近截止时间唤醒；存在属性动画时沿正常呈现节奏采样，暂停和平台挂起不消耗等待时长。快进沿既有有限会话活动规则处理。接口为 Reel native ABI 1，不改变 B01/ASB。

桌面播放器的初始客户区默认跟随作品首次配置的逻辑画布尺寸；未配置画布时使用 1280×720。Rust `StoryWindowOptions.logical_size: Option<[u32; 2]>` 可以显式覆盖窗口尺寸，`canvas_size` 仍只定义逻辑画布。首个画布应用后不再自动调整窗口，玩家主动缩放与后续画布变更继续沿 contain 绘制/输入变换处理。该策略仅属于窗口适配器；B01/ASB、game.configure native ABI 3 和库宿主保持不变，无新依赖。验证包含实际桌面启动尺寸、显式覆盖及首次配置后的用户缩放。

宿主对象退休时，依赖它的 derive 解除全部输入订阅并冻结在最后一次成功的值，已排队的计算同时失效；直接访问退休对象仍报 RetiredHost。移除最后一个属性消费者会递归解除其派生输入订阅，保留的派生引用再次绑定时恢复更新；仍有共享消费者的计算保持活跃，已退休输入的冻结值不再恢复计算。断开沿实际订阅边迭代执行，不扫描整个堆；测试覆盖条件外观、共享与重新绑定、排队后退休、GC 与源码/ASB。

### 按业务身份复用的作者列表

滚动组合公开 `scroll_x/scroll_y` 可写可绑定位置，以及 `viewport_width/viewport_height/content_width/content_height` 只读可观察尺寸。实际布局将位置钳制到内容范围，滚轮、触摸与焦点跟随回写同一位置；只有改变的数据发布通知。`content_align: "start" | "center" | "end"` 在滚动轴上对齐短内容，默认 start；follow_end 继续保留上滚阅读位置。位置更新只合成，不重新测量文字。`input(on_pointer: callback)` 交付主指针拖动的 `{target,x,y,phase}`，坐标来自已接受呈现的节点局部坐标，phase 为 pressed/moved/released/cancelled；按下捕获，失焦/隐藏/禁用取消，回调归节点所有。作者以普通节点和绑定组合轨道、滑块和箭头，不固定滚动条外观。范围为 reel-ui/reel-engine/reel-player、示例与双语指南；scroll ABI 3、input ABI 7，B01/ASB 不变，无新依赖。验证拖动及模态取消、双向位置绑定、短内容对齐、追加跟随及连续滚动期间布局次数不增长。

key 接受任意 Aside 值，标量保留类型、字符串按内容、其他对象按完整堆身份比较；浮点正负零为同一 key，NaN 使用稳定的位模式顺序。不调用作者比较器或对象钩子。引擎返回的不透明选项 id 可直接作 key；列表保留原键的强引用，GC 与槽复用不改变行身份。

`list(items, key:, build:, ...NodeProps)` 是 Reel native ABI 2，B01/ASB 不变。来源函数与 key 函数沿既有 NoWait/reactive/native continuation 执行；每项构造可让出，候选数组、行与节点由 VM 根追踪。行工厂通过 VM 的 untracked capture 隔离外围 view 依赖，嵌套 derive/list 仍独立记录读取，故障和取消沿 NativeFrame 恢复上层捕获。稳定行公开只读可观察 key/value/index，保留 key 时只更新 value/index，局部订阅驱动外观。节点复用同一 Stage；子节点批量协调先验证单父、重复与环，再重排保留身份并退休移除的树。已呈现输入仍按实际接受帧和节点寿命判断。

结构协调使用排序索引和二分查找，O(n log n)，不逐项线性搜索全表；只为新 key 执行组件工厂。普通行字段更新由 bind/derive 完成，列表无每帧扫描。排序及最后的树提交为同步批次；索引临时内存通过 Aside 的 HeapBudgetUsage::reserve 归入同一预算，HeapReservation 析构释放计账，持续状态全在 VM 堆。失败/取消不留下 Rust 侧工作副本或独立根。当前列表保留全部行实例，滚动使用既有 scroll；可见范围虚拟化不属于这个接口的已实现能力。示例见 [author_list.aside](../examples/author_list.aside)。

### 角色对白与公共播放时钟

富文本采用显式 `rich(markup)` 作者入口，普通字符串保持字面含义。句内 `[b]`、`[i]`、`[u]`、`[s]`、`[size=32]`、`[color=#ffd580]`、`[ruby=ほし]星[/ruby]`、`[em]` 和 `[link=entry]` 使用成对嵌套标记，`[[` 表示字面左方括号。解析产生平台无关的不可变全文、UTF-8 样式范围、注音与链接身份，共用于 UI、对白和历史；Aside 不依赖这些类型。字体布局复用 reel-text 的范围样式和 inline box，注音与着重号参与行高，ruby 基文与注音作为一个换行单位。揭示仍按基文的字素推进，标记和注音不消耗剧情字符数；链接只在已显示的命中区域交付 `on_link`，不自动打开 URL。段落通过 text_align 与 line_height 设置对齐和行高，舞台位置、尺寸和裁剪控制 ADV/NVL 布局。源码与 ASB 共用此接口；段落按可用宽度换行，逐字显现保留布局缓存，链接使用已呈现的输入几何和节点所有权。错误标记保留 rich 调用位置与标记字节偏移；rich_escape 为显式标记插值转义左方括号。

对白呈现模型逐行公开只读 `id/speaker_id/speaker_name/content_id/text/reveal/read`。旁白 speaker_id 为 null；read 保存该行提交前的已读状态，NVL 累积行不随之后的推进改色。顶层 speaker_id/content_id 与姓名、文本一样投影第一行，read 仍表示整组提交前均已读。交互结束或进入选择时保留所显示对白的身份与 read，直到下一组对白替换；active 单独表示是否等待对白推进。作者用 derive 选择颜色，用普通节点与 tween 组织退出动画，动画不会修改阅读进度。身份字符串在行发布时分配，与同批历史共享，不参与逐字显现更新。

`character(name, id: null)` 创建不可变角色；省略 id 使用 name，显式 id 区分同名角色。角色可直接调用并等待一句，`.line(text)` 返回不可变行描述；`say(lines)` 原子提交非空 ADV 行组并共用一次揭示/推进交互。角色名、身份、完整文本按提交时保存进历史；行描述可复用。旁白沿相同会话协议，回调不能抢占主剧情对白。可信宿主可调用协议由独立 Aside 提供，角色不引入语言特殊语法。组解码按工作额度让出，失败不产生部分历史。

`game.configure(text_speed: null)` 配置每秒揭示字素数，默认 30，0 为即时完整显示，省略保留设置。播放器以公共单调逻辑时间播放，挂起、资源等待及正在配对的输入不积累欠账；完成后停止定时唤醒。文本始终按完整内容整形，时间推进只修改揭示范围。窗口只适配时间和唤醒，Rust 嵌入宿主使用同一播放逻辑。源码/ASB 示例见 [author_dialogue.aside](../examples/author_dialogue.aside)。

### 当前作者舞台实现合同

选择控件复用共同舞台：`checkbox(label: null, value: false, on_change: null, ...)` 与 toggle 共用布尔值、双向绑定及按钮激活，仅装饰和可访问性角色不同。`radio(option, label: null, value: null, on_change: null, ...)` 将选项绑定到一个业务值；同组各项的 value 连接同一模型字段，激活时写入 option，重复选择不回调，也不取消选择。option 可更新或绑定，旧选项画面的激活不能选择更新后的 option。比较沿用 list 的业务 key 规则，允许标量、字符串及对象身份；未匹配值表示无项选中。导航使用共同方向/Tab 焦点与确认输入，禁用、模态和退休沿共同生命周期。两者支持 children 与 decorated，自定义外观读取 value/option 和既有交互状态。范围为 reel-ui/reel-engine/reel-player、示例和双语指南；新增 native ABI 1，B01/ASB 不变，无新依赖。验收源码/ASB 双向绑定、互斥、动态选项、输入消费和取消；值变化保留控件身份，不重新排版未变文字。

`input(bindings: null, on_action: null, on_key: null, ...NodeProps)` 可在现有输入节点订阅逐次键盘事件。on_key 收到 target、code（物理键）、key（逻辑键）、phase（pressed/repeated/released/cancelled）、shift/ctrl/alt/meta；每个按键单独配对，系统重复不丢弃。null bindings 表示没有动作映射。聚焦控件优先消费自己的按键，普通控件未消费的键盘事件向祖先 on_key 冒泡；文本编辑控件保留全部键盘与 IME，未消费的编辑按键也不进入游戏回调。动作映射仍要求输入节点拥有焦点或根输入兜底。失焦、隐藏、禁用、模态替换与节点退休撤销旧按键；事件沿接受帧准入与节点任务所有权交付。宿主注入使用公共 KeyboardEvent，无原生依赖；空闲不轮询。此变更覆盖 reel-ui/stage、reel-player 输入路由与 reel-engine 属性/回调，不增加依赖；input native ABI 5，B01/ASB 不变。验证多键交错、重复、焦点取消和编辑控件隔离。

`text_input(value: "", on_change: null, on_submit: null, ...NodeProps)` 是共同舞台的单行文本编辑节点，value 支持可写 bind。编辑复用 reel-text 的 PlainEditor；提交文本与 IME 预编辑分别管理，预编辑只存在于控件，提交才写回模型。on_change/on_submit 收到 target/value，Enter 提交而不推进剧情；失焦、隐藏和禁用撤销预编辑。外部值替换结束当前预编辑，普通值回显保留光标和选区。输入事件携带编辑前值，避免用旧呈现覆盖脚本已修改的文本。控件按需布局，空闲不定时轮询。字体、颜色、背景、尺寸、变换和输入范围沿共同节点；不规定字符数量上限。作者赋值须为单行字符串；玩家粘贴或输入的换行转换为空格。平台适配器转发公共 IME 事件和候选框区域，系统负责转换与候选词；原生窗口类型不进入 UI 或 Aside。桌面复制、剪切和粘贴由可选适配器复用固定版本 arboard 3.6.1。新增节点 ABI 1，B01/ASB 不变。

手柄输入通过公共设备标识、标准按钮/轴和动作映射接入；宿主可直接注入规范化输入，Gilrs 只属于可选平台适配器。ActionMap 汇总同一动作的多个设备/绑定，公开当前值与 Pressed/Changed/Released/Cancelled；松开一个绑定不会清除仍被另一个绑定保持的动作。断连或重置撤销该来源，重映射用新映射替换并显式取消旧状态。导航另将模拟量转换为带迟滞的方向/确认/返回，重复只由单调时钟与下次截止时间驱动，空闲没有逐帧输入轮询。

UI 的语义 Navigation 事件沿焦点、已呈现几何和 modal 作用域分发；确认的按下/松开必须属于同一仍有效目标，失焦、清空呈现或作用域变化取消配对。标准按钮使用既有 Click 行为，方向优先交给控件，再按已呈现位置移动作用域内焦点；焦点请求沿原滚动协议交付，长剧情选项列表滚入焦点项并在新画面接受前撤销旧点击配对；返回交给当前 UI 消费，不能在窗口层提前关闭应用。剧情确认沿既有推进准入，按钮仍消费输入。键盘保留文本/IME语义，未消费的方向键使用同一导航路径。

Windows/Linux/macOS 手柄后端复用锁定 Gilrs 0.11.2，独立 gamepad feature 不拉入 Winit/WGPU；内置 window 组合启用它。桌面后台阻塞等待事件，经有界队列和唤醒通知交付，关闭取消接收端且不等待设备线程。平台不支持该后端时仍可注入公共输入，不能把桌面编译或注入事件当成各平台实体设备验证。

作者输入使用共同 `input(bindings: {...}, on_action: callback) { children }` 节点，按 stack 布局。bindings 的键是动作名，值为 binding 描述的数组；`binding(control, scale: 1, dead_zone: 0)` 使用物理键名（KeyA、Space 等）、标准手柄按钮（South、DpadLeft 等）、轴（LeftX 等）或指针按钮（PointerPrimary 等）。同一节点复用 ActionMap 聚合多个绑定和设备。`node.value(action)` 返回当前保持值；回调事件为 target/action/value/phase，phase 为 pressed/changed/released/cancelled。输入节点自身获得焦点后接收键盘和手柄动作；子控件取得焦点会接管输入并取消父输入节点的保持，键盘和手柄保持一致。声明为 modal 的输入子树自动接管焦点。消费发生于同步路由，业务回调仍是可暂停的节点任务。
输入节点的 bindings 替换时原子撤销旧保持值，后续事件使用新描述；绑定对象按赋值更新，不隐式观察嵌套表修改。失焦、清除已呈现画面、模态切换、隐藏或禁用撤销动作并发出 cancelled；退休则撤销节点任务并释放状态。取消只清理旧所有权，不产生点击或剧情推进。原生控制事件先路由作者输入；已消费的控制不再变为默认菜单导航，设备断连仍清理双方状态。Rust 宿主通过相同 NodeKind、NodeAction 和公共输入事件使用这一能力。设备身份以 Keyboard、Pointer(PointerInfo)、Gamepad(id) 枚举区分，数字不会跨来源碰撞。原始 Input 事件返回作用域内实际动作消费结果，未绑定控制再经过同一模态内的语义导航。AsideSessionUi 的 presented/clear_presented 返回 Result，交付呈现切换引发的取消；宿主据此唤醒新任务。


指针输入按 `PointerInfo` 的设备、类型与触点身份保持独立捕获及命中路径；不同触点可以同时操作不同控件，终止事件只释放对应触点。事件上下文中的 active/hovered 指当前触点，绘制和普通查询汇总仍在交互的指针。同一个普通控件的拖动由先捕获的触点拥有，额外触点不能接管；窗口失焦、挂起、呈现撤销、禁用、退休和模态切换逐一取消受影响捕获，取消保留原触点身份且只交付一次。触摸结束清除悬停与触点记录，不积累历史触点。路由沿现有已呈现几何，无稳定移动期间分配，也不按触点扫描控件树。PresentationBridge::handle_pointer_event 返回 Result；新触点的路由和记录在分配、分发前计入已有 max_bridge_bytes，预算不足返回 InputBudgetExceeded 并保持既有捕获。终止释放触点路径，容量按实际占用计量，不另设触点数量上限。

Winit 适配器把原生触摸位置、压力和设备/触点身份转为公共 PointerEvent；库宿主可以直接提交该公共事件，不依赖窗口或 GPU。触摸点击沿现有按钮与剧情输入配对，拖动沿原控件行为。此合同不把平台未提供的手势推测为点击。

共同节点的 `modal: false` 属性控制输入作用域，绘制层序仍由节点组合顺序决定。最上层可见 modal 子树独占输入，范围之外及未被子树处理的指针、键盘和可访问性动作都不能继续触发下层 UI 或剧情；禁用仍隔离下层，隐藏或退休释放作用域。Tab/Shift-Tab 在作用域内循环；进入时选择可用焦点，嵌套或替换时保存该作用域的焦点，关闭后恢复仍有效的先前焦点。新作用域撤销外侧指针捕获，按既有协议只交付一次取消。

作用域随实际接受的呈现几何切换；待提交或被拒绝的画面不提前改变输入范围。撤销呈现期间不交付交互，保留焦点恢复身份供下一有效呈现使用。原生 UI 的 Live 几何模式使用同一作用域规则。Rust 控件通过公共上下文设置 modal，Aside 节点映射到同一个字段与输入分发层；回调进入 VM 前仍检查当前节点的有效性及作用域，防止过期点击启动任务。已呈现几何在原有导出遍历中记录作用域；指针事件只检查目标祖先链，不重新扫描整棵树。Live 模式仅在模态、可见性和结构变化时重选作用域。焦点恢复只保留每个存活控件上的身份，不积累历史弹窗记录。VM 点击准入按反向绘制顺序读取当前 visible/modal 字段，找到首个作用域即结束，不复制节点文本或完整属性。

作者按钮实现合同：`button(label: null, on_click: null, children: [], ...)` 使用共同节点属性和子树；省略 label 时由 children 定义外观。button 作为 stack 排列内容，label 是居中的文本内容，子节点按作者顺序叠放。`enabled` 是所有节点共享的可写布尔属性，默认 true，禁用容器会禁用整棵交互子树。构造、属性更新、局部添加和退休沿用同一 Stage，不建立独立按钮节点体系。

按钮复用 reel-ui Button 的指针、焦点、键盘和可访问性激活行为；该行为提取为共享实现，普通 Rust Button 与作者按钮共同调用。接受的 click 通过原有 UI 输入槽交付，包含稳定 NodeId；仅最后接受的呈现和仍可交互的活节点可触发，控件消费的输入不继续推进对白。回调参数是普通事件对象，`event.target` 为原节点；on_click 可赋新闭包或 null。回调不在控件分发或 Rust native 栈中执行。

键盘激活由同一个按钮配对 Enter/空格的按下与松开；失焦、窗口失焦、禁用和隐藏取消尚未完成的按键。切换模态作用域后的孤立松开或重复按下不能激活新焦点按钮，激活键的两端均由按钮消费。

会话适配层在真正等待或任务结束后选择下一可运行任务，复用 VM start_closure、NativeFrame、等待、故障与 GC。同步脚本段连续执行；预算耗尽让出宿主线程，恢复后仍执行原任务，避免属性读改写在无显式等待处相互覆盖。每次接受的事件创建受 VM 资源预算管理的任务和可复用存储的会话 owner，成功返回后释放交付并退休 owner；没有固定作者事件数量限制，也不累积历史任务。任务可等待活动；活动等待维护多个消费者，消费者取消不取消共享活动，结算保留订阅顺序且用线性压缩移除失效等待。回调中省略 owner 的 run 默认归事件节点，主任务仍默认归会话。节点退休在下一次脚本推进前硬撤销所属回调，取消等待并分片清理任务，不能执行退休后的下一条脚本。关闭会话显式汇总主任务及所有回调的清理义务。narrator/choose 仍由主剧情拥有，回调调用它们报告可捕获的上下文错误，不夺取当前对白。

主入口返回后，已挂载的交互节点、正在运行的活动或回调继续维持会话；没有可运行工作时休眠。全部交互与活动结束后按原自然结束路径保留最后画面。单个回调未捕获的故障保留真实源码和完整 VM 故障交付，并暂停会话，供检查和显式关闭清理。

本次范围：reel-ui 的 stage 与共享按钮行为、reel-engine 的绑定和任务调度、reel-player 的已呈现输入和生命周期、reel-cli 的源码/ASB 合同，以及规范、示例、双语指南。依赖 #74/#78/#80/#82；B01/ASB v1 和 native ABI v1 保持。仅桌面适配器增加锁定的 ui-events-winit 0.3.0，用于原生键盘事件转换；共同输入、舞台和调度不依赖 Winit 或 WGPU。验收覆盖真实指针配对/消费、禁用祖先、捕获闭包、并发等待、入口返回后的交互、回调自移除与替换、旧帧/旧任务过滤、关闭和源码故障；成本检查空闲不轮询 VM、保留控件身份及反复点击不积累 owner/任务/根。

`reel-player::story_runtime::StoryRuntime` 在无桌面 feature 时同样可用，供任意宿主复用 VM、活动、GC 和关闭调度。宿主传入单调时钟与执行预算，在输入提交后调用 wake_input，并根据 needs_tick 安排唤醒；不把 Winit 事件循环或 WGPU 表面作为调度前提。当前 window feature 只表示内置 Winit + WGPU 组合，其他组合使用公共输入、PresentationBridge 和 RenderBackend 合同；这些接口不得暴露原生窗口或设备类型。关闭该 feature 的依赖树检查是本轮交付的一部分。

作者滚动容器：`scroll(axis: "vertical", follow_end: false, ...NodeProps) { children }` 使用共同节点。axis 为 vertical/horizontal/both；横向滚动按行排列，其余按列排列。视口使用显式或可用宽高，滚动轴按自然内容测量，始终裁剪。follow_end 在原本位于末端时跟随追加内容，作者/玩家离开末端后保留当前位置；内容缩短时夹取有效位置。滚动只改变 compose 位移，不重建节点或文字布局。源码/ASB 与 Rust Stage 共用实现，新 scroll native ABI 1，既有 B01/ASB 不变。

滚轮和触摸沿共享 ScrollState，最近可消费的滚动祖先优先。点击类捕获允许父滚动容器在触摸超过 8 个逻辑像素的方向阈值后接管，并先向原控件发送 Cancel；滑条等独占捕获保持原所有者。取消、禁用、退休和模态切换沿既有指针生命周期。焦点请求逐层按实际滚动位移修正目标矩形，支持嵌套容器。默认剧情视图复用相同行为。关闭默认对白界面后，未被控件消费的轻点沿现有会话推进协议；拖动接管会取消该轻点，不推进剧情。滚动位移由 UI 管理，不依赖脚本逐帧更新或节点创建，不触发文字排版；窗口和 GPU 不进入公共滚动接口。

动态子树合同：`parent.add(child, index: null)` 将未挂载、无父节点的子树插入 row/column/stack，默认末尾，显式位置为 0..=子节点数，返回原 child。拒绝成环、重复拥有、越界和向叶节点添加，失败保持原树。`node.remove()` 从父级或舞台移除并退休整棵子树；`container.clear()` 退休全部子节点而保留容器。退休清除脚本私有强边、图片请求和目标活动的有效性；已退休引用不能重新挂载。提取的方法值保留接收者身份，重复调用已提取的 remove 可安全返回。

窗口调度交替推进 VM 和增量 GC；已失效对象仍有载荷等待释放时，先推进有额度的 GC 片，再恢复脚本分配。GC 片同样服从窗口截止时间，不通过增加堆预算掩盖释放积压，也不在一次调用中强制完成整轮回收。

本次范围为 aside-vm、aside-host、reel-ui stage、reel-engine、reel-player 控件同步及源码/ASB 合同；B01/ASB v1、native ABI v1 保持，无新依赖。绑定 native 方法直接进入既有 NativeFrame，接收者是受 GC 跟踪的首参数，具名/默认参数、等待与效果检查复用普通调用。结构修改的工作和临时存储随受影响兄弟/子树增长；呈现端按 NodeId 保留原控件，只建立新增子树、删除退休控件和更新顺序。未改变的文字不重新整形，不用全场景替换模拟局部修改。跨父移动、通用事件订阅、scope 与快照另行实现。

Reel 源码编译入口将当前注册的 reel::prelude 接口放入外围宿主名称环境，作者无需逐文件导入。模块声明、局部绑定及显式导入优先；普通 use 与别名可取得被遮蔽的接口。名称绑定来自同一宿主注册描述，check/build/dev 与构建出的 ASB 使用同一链接合同；不向源码注入前缀，诊断保留原 UTF-8 范围与行列位置。Aside 的默认宿主环境为空，不依赖 Reel；该默认由 reel-engine 的 AnalysisOptions.hosts 配置提供，不改变语言、VM 或 B01/ASB v1。


图片节点合同：`image(source, fit: "contain", ...)` 使用共同节点属性，source 为非空文件路径，fit 为 contain/cover/stretch。source、fit 可直接修改；省略宽高时使用图片固有逻辑尺寸，只指定一边时保持固有宽高比，指定两边时由 fit 决定缩放与裁剪。构造及赋值只更新舞台描述，不执行文件 IO；仅挂载子树请求资源。

player 使用锁定 image crate 的 PNG/JPEG 解码器、已有 Image 控件和 ResourceLease。Assets 共用 AssetIo 与 WorkExecutor，默认组装后台 DirectoryIo 和 ImageExecutor；宿主可通过 with_assets 注入自己的适配器。重复 source 共享不可变像素；加载请求随最新舞台更新，过期结果不发布。新舞台所需图片与配置字体全部就绪后整体发布，其间保留上一完整画面并禁用其剧情输入；后台完成唤醒窗口，不通过连续轮询驱动绘制。换源和退休释放缓存所有权，已接受帧的租约按原有提交寿命释放。关闭不等待文件 IO/解码，撤销接收端及请求，后台任务持有的内存预算随退出释放。文件读取和解码不进入绘制热路径，不逐帧复制像素。

资源路径默认相对于 dev 的源码文件或 run 的 ASB 文件所在目录；`--assets <directory>` 显式选择根目录，库宿主通过 AssetOptions 配置，并通过 AsideSessionUi::set_image_waker 安装后台完成通知。同一路径在驻留期间代表同一不可变资源版本，退休后重新挂载会重新读取；不监视磁盘文件变化。加载失败结束本次窗口运行，并报告图片路径和该 image 节点构造位置；它属于异步宿主资源错误，属性类型/fit 错误仍在赋值处同步抛出。嵌入 ASB 的源码位置只用于诊断，不决定运行机器的资源目录。

显式取消清空当前作者舞台并撤销图片请求，随后同步发布取消状态；这条路径不再请求已关闭的加载器。自然结束保留最后画面，直到宿主关闭。库宿主与窗口使用同一生命周期。

GPU 回放在一次帧准备内复用已经完成合成的离屏目标；复用前清空，仍在父组、遮罩或交叉淡化中使用的目标不得回收。预算按实际分配的纹理峰值计费，并保留到 GPU 提交完成，不能在记录完绘制命令时提前退还。同级裁剪与分组不再逐项累积整窗纹理；这一内部变更不改变帧格式、采样质量或默认预算。

图片实现遵循 reel-ui 舞台/资源快照、reel-engine 宿主绑定、reel-player 平台组装与 reel-cli 作者工具的模块边界；保持 B01/ASB v1，宿主 image 使用 ABI v1。图片使用可配置的驻留像素预算与单次解码工作预算，无额外作者节点数量上限；解码器私有暂存由 image 的 limits 约束，不能声称为逐字节引擎预算。验收覆盖源码/ASB 的真实图片帧、PNG/JPEG、固有尺寸/fit、共享与换源退休、加载错误及关闭后迟到结果；成本检查关注解码次数、共享像素、空闲时无加载/轮询与最终预算释放。

脚本组合、共享节点和窗口宿主已接通。`reel-ui::stage` 持有唯一的作者节点模型，engine 管理脚本值和会话寿命，player 将同一模型投影到既有控件、呈现桥和渲染器。`text`、`image`、`row`、`column`、`stack` 返回具有稳定身份的未挂载节点；`stage.replace(root)` 挂载新根并结束旧子树。普通函数可返回节点，尾随 children、条件和集合循环按语言的收集规则组合子树。可运行源码为 [author_stage.aside](../examples/author_stage.aside)。当前验证覆盖源码、ASB、VM、实际控件和帧记录生成；Windows 上另使用 AMD Radeon RX 7900 GRE / Vulkan 完成中文作者画面的离屏 GPU 读回。此证据不覆盖原生窗口生命周期和设备热插拔。

节点公开位置 x/y、可选 width/height、opacity、visible、clip、gap、padding、font_size、color 和 background。未指定宽高采用内容布局；位置默认 0，opacity 默认 1，visible 默认 true，clip 默认 false，gap/padding 默认 0，font_size 默认 24，color 默认白色，background 默认透明。row/column 顺序排布，stack 按书写顺序叠放；x/y 是布局位置上的局部偏移。属性赋值修改原节点，既有引用继续指向同一对象。节点身份、属性和子对象引用由真正的宿主对象承载，不能靠复制普通 table 冒充节点。单父关系、节点退休和根对象保活在挂载/会话边界处理。

节点变换合同：所有基础节点共同提供 rotation、scale_x/scale_y、anchor_x/anchor_y。rotation 使用顺时针角度，默认 0；缩放默认 1，负数翻转，0 可折叠；锚点按节点布局宽高的比例取值，默认左上角 (0,0)，(0.5,0.5) 表示中心。锚点不限制在节点内部。布局位置加 x/y 表示锚点所在的父坐标位置；变换为 T(layout + x/y) × R(rotation) × S(scale_x,scale_y) × T(-anchor × size)。角度、缩放、锚点均要求有限数值；角度不钳制在一圈内。width/height 与容器排布使用变换前尺寸，变换作用于整棵子树、裁剪和呈现几何。奇异变换沿现有几何规则不参与命中；零缩放不退休节点。

实现复用 Kurbo Affine 和 reel-ui 的 compose/呈现几何，不增加第二套变换或命中系统。ComposeCtx 提供基于完成布局尺寸设置子控件变换的接口；x/y、rotation、scale、anchor 和 opacity 更新保留控件身份，不重新执行布局/文字整形。内容、尺寸、padding/gap、字体大小、显隐及裁剪更新按实际影响请求布局。范围为 reel-ui、reel-engine 宿主绑定、reel-player 投影、reel-cli 验证及规范/双语示例；B01/ASB v1 和节点宿主 ABI v1 保持，无新增依赖。验收覆盖源码/ASB、嵌套矩阵、锚点随尺寸变化、负/零缩放、裁剪/呈现几何、错误位置及实际 GPU 读回，并记录纯合成编辑期间布局次数不增长。

窗口入口为 `reel_player::shell::run_story(CompiledStory, StoryWindowOptions)`。选项包含标题、资源来源、locale、可选的原生窗口 logical_size 和逻辑 canvas_size；字体由作品的 game.configure 提供。窗口复用 SessionUi、PresentationBridge 和现有渲染路径，交替推进 VM 与增量 GC 工作片；等待玩家输入时完成当前回收周期后休眠，输入完成后继续。正常剧情结束保留画面；关闭执行任务取消及清理后退出。ResourceBlocked 保留执行状态，允许 GC 后重试，持续阻塞时停止忙等。源码和 ASB 入口共用编译/注册描述及运行宿主。新能力使用已有资源所有权和预算，不再叠加仅为演示设定的行数、选项或节点上限。

作者命令 `reel check`、`reel build` 和 `reel dev` 共用 engine 的宿主注册与源码编译入口。check/build 保持不执行脚本，允许已解析的 Reel 原生接口；ASB v1 保存既有 B01 原生声明，运行时仍由 SessionVm 的宿主注册提供能力。`reel dev <source.aside>` 运行源码，`reel run <file.asb>` 直接解码并运行构建产物，不重编译内嵌源码。window feature 提供窗口命令，未编入时说明所需构建命令。默认源码入口为 main::main；源码文件或工程目录按 main/--entry 加载实际多源码模块。模块共用一个字节码程序、固定导入和独立的会话全局环境，函数先链接，顶层表达式按依赖/SCC 顺序初始化。配置标题与 contain 画布沿共同 UI 变换处理绘制和输入。资源包与 release.json 仍由发布链路交付。

一次窗口推进在既有 2 ms deadline 内连续处理同步调用与 VM/GC 工作，直到真实等待、终态或时间片耗尽，再同步画面。创建多个组件或同步替换场景不会被拆成每个 native 一次重绘；时间片包含宿主调用耗时。没有 deadline 的库级 tick 保留单片推进语义。

节点组件的父子私有引用双向保活：持有任一节点都保持其未挂载组件有效，GC 不改变仍被作者持有节点的行为。显式替换舞台或撤销会话仍整体退休旧树，并清除全部私有强边。Host 属性通过只读堆的同步纯验证器在原赋值提交前验证，失败从作者赋值位置抛可捕获错误，原属性保持不变；呈现同步只解码并发布已验证值，不承担作者错误的延迟检测。

RenderRootOptions.default_font 是正式默认字体输入，注册其字体族为 SystemUi 默认族，适用于字体实际覆盖的全部文字；它不再使用仅限 Latin 的测试回退。字体资源拥有整份字体文件或集合，FrameGlyphRun 单独携带 face 索引，各个 face 共享同一资源和内存；默认族按文件中的索引顺序选取，不受哈希遍历顺序影响。

剧情按阅读顺序运行，在对话和选择处等待；角色身份、历史记录与显示组件分开。画面和 UI 共用节点、资源、布局、变换、裁剪和效果组。长期存在的节点通过属性更新，避免每次输入重建整个界面。参考 Ren’Py 的交互与屏幕复用经验。[1][2][3]
核心提供存档、设置、历史、对话与选择的数据模型及操作接口。标题页、设置页和存档页由作者组合；项目脚手架只生成配置和入口，不附带一套完整内置 UI。用针对性契约用例验证这些能力，作者文档中的片段用于说明组合方式。
初期使用一个工作区，先落地默认 winit 窗口适配和 wgpu 渲染器，窗口管理与渲染后端可以独立选择。完整目标包含视频、可替换的软件/硬件后端、文字与本地化、可访问性、二维/透视演出及扩展服务；阶段安排按依赖推进。静态子树缓存和增量快照是保持同一行为的优化。现有 ons-rs、krkr-rs 用作结构参考。[4]

## 二 模块职责与依赖
工作区按职责划分，语言包可独立发布。
- reel-core：平台无关的资源/请求身份、预算/结果类型、AssetIo、Clock 与 WorkExecutor 契约；只含共享数据和基础服务接口。
- aside-vm：语言值、对象、调用帧、模块与执行接口。
- aside-compiler：解析、分析、字节码及源码定位，依赖 VM 公共格式。
- reel-ui：控件、布局、属性、交互与舞台桥接。core/app/layout/properties/widgets/theme 管理 UI 基础，bridge/frame/resources 管理呈现接受、可移植帧与资源租约；passes/tree_arena 为私有实现。
- reel-render：后端接口、资源上传、缓存与分组合成，读取 UI 提交结果。
- reel-media：Kira 适配和播放控制，视频作为可选模块。
- reel-engine：会话、剧情任务、宿主绑定、资源与持久化协调。
- reel-player：平台适配、窗口与事件循环，组装引擎、渲染和媒体输出。
- reel-cli：检查、开发运行和构建，调用编译器与播放器库入口。
依赖方向固定为 ui→core，render→ui/core，media→core，player→ui/render/core；后续 engine→aside-vm/ui/media/core，player→engine，cli→compiler/player，compiler→aside-vm。Aside 保持独立于所有 reel 包。语言包不依赖窗口、舞台或媒体。业务数据类型只在所属模块定义一次，跨线程传递拥有自身数据的消息或线程安全只读缓冲，不传 VM 堆引用。渲染器通过资源 ID 与版本取得内容。
维护中的库统一属于工作区 crate，不保留 third_party 或 crates.io patch 层。reel-text 负责文字整形、布局及 ICU 中日分词与复杂文字换行；reel-audio 负责混音、声音与退休数据的移交，设备输出由 reel-media 唯一负责；aside-syntax 负责不可变语法树、源码位置及共享节点所有权。三者分别源自 Parley、Kira、rowan，许可证及简洁来源说明随 crate 保存，后续按 Reel 的职责和接口直接维护，不承担上游目录、版本或补丁同步要求。
aside-syntax 在所有权层通过原子引用计数取得唯一释放权，并迭代释放深树，覆盖独立节点克隆和跨线程最后引用。其继承的底层指针实现封装在语法树 crate 内；aside-compiler、reel-ui、reel-text 和 reel-audio 的生产代码保持 unsafe_code=forbid。语言仍使用单一递归下降/Pratt parser，长左结合运算和后缀链按显式工作表处理。
通用库使用锁定的普通依赖，包括 winit、wgpu、image、serde、atomic-write-file、codespan-reporting、Logos 和 lsp-server；文本、集合与数学分别复用 Unicode 库、indexmap、glam，VM 对象槽使用 slotmap。视频默认 ffmpeg-next/ffmpeg-sys-next 与锁定 FFmpeg，资源使用独立 zstd 块、Stored 媒体与 BLAKE3。借用的 UI 控件、布局与呈现几何只维护在 reel-ui；不引入重复的窗口、设备输出或 UI 类型实现。
Quirrel 的 C++ 实现和独立兼容运行器退出工作区，完整原测试输入、预期输出与原 runner 参照保留在 aside-compiler 的测试 fixtures 中。迁移用例由同一 Aside 编译器生成 B01，编码/解码后交给真实 VM，在普通工作片和极小工作片交错 GC 下分别核对原断言与输出。解析通过、原 Quirrel 通过和 Aside 执行通过分别记录；未迁移用例不能删除或计为通过。来源固定于提交 19f17df9b6ee43c91ef5c635fd45970ec76c5dbe。[6]
UI 代码最初借用自 Masonry 基线 271a27a6d4a930f7878d404f9014e3c50a3a9b88。[5] 当前仅维护 crates/reel-ui 一个 UI crate，版本跟随 Reel workspace；原 masonry、masonry_core、tree_arena、reel-stage 的实现按职责合为内部模块，不保留兼容转发包。安全树为私有模块，文档截图直接引用，不再保留 include_doc_path 宏包。许可、版权及必要来源说明保留；不要求与上游目录、版本或源码哈希持续一致。reel-render/reel-player 分别承担后端与平台组装，UI 不反向依赖它们。
借用实现由对应 workspace crate 自主维护，保留原许可证、版权及简洁来源说明。来源记录不构成持续同步源码、目录或哈希的约束；测试迁移保留原输入及 oracle，不把原编译器专属输出当作 Aside 必须模仿的 API。
播放器的 source 能力选择性引入编译器；精简版本只加载字节码。hot-reload 只开启模块替换协议，可接收外部编译产物。编译器、监视器和对照运行时留在开发侧，精简构建检查实际依赖树。[7]

## 三 平台接口与移植边界
### HAL 与公共呈现（#83）

内置桌面使用公共 PlatformHost 与 Presentation，其他平台复用同一会话、输入、记录、已接受帧与生命周期。范围为 reel-render 后端合同/既有 WGPU 实现、reel-player 公共呈现、运行时计时与 Winit 适配，以及合同检查、规范和双语指南。复用 reel-core 的时钟合同，保持 Aside、B01/ASB v1 和 native ABI v1；Rust 宿主/渲染接口直接更新，不保留兼容层，无新增依赖。

PlatformHost 的当前必需命令为 request_redraw、set_wake_deadline、before_present、request_exit，时钟沿用 Clock，后台通知沿用 WakeSignal。原生事件由适配器转换后进入共同 UI/会话；窗口创建及原生表面创建仍在组装代码。剪贴板、IME、系统可访问性属于独立可选平台服务，后续实现时给出真实能力与结果；本轮不以空成功实现这些服务。

RenderBackend 保留配置、呈现、完成轮询和挂起责任，使用具体错误类型及统一的拒绝/设备丢失/表面失效分类。呈现通过宿主的 before_present 通知后才交给平台。RenderFrame 已拥有全部 CPU 资源租约；后端可以在同一次提交内上传，独立 upload 是可选预热能力，上传额度为零的后端明确拒绝而不影响普通帧资源。资源生产者通过 ResourceRegistry 退休资源；后端 retire 退休其持有的资源，既有已接受帧租约的重绘仍合法。无持久 GPU 缓存的后端按提交保留和释放帧租约。完成轮询不得丢失超过本次额度的结果，完成通知和呈现接受保持独立。

公共呈现协调器管理画面更改与曝光重绘、候选几何、已接受帧、失败后的重试和表面代数。任何失败/跳过都结算对应候选，不能积累无主候选或发布未呈现输入；永久拒绝后只重绘最后已接受帧，提交完成才解除临时压力。原生 GPU/窗口类型只出现在各自适配模块。无桌面宿主与默认桌面调用同一协调器。

会话逻辑时间排除宿主挂起区间；VM/GC 截止时间继续使用真实单调时间。恢复先更换表面代数与输入几何，再恢复逻辑时间；关闭即使在挂起期间仍分片清理任务。验证真实源码的输入、动画、挂起恢复、拒绝/旧帧和关闭，保留既有呈现、预算和终态合同；无桌面/启用适配器依赖树、debug/release、Clippy 分别检查。关注空闲无轮询、无额外逐帧像素副本、完成事件不重复分配队列，以及租约实际释放；CPU、软件 GPU 与原生窗口证据分别陈述。

窗口和渲染分别选择，默认组合为 WinitHost 与 WgpuBackend。reel-player 的 winit feature 单独提供 platform::winit::WinitHost，不依赖 WGPU；window feature 组装内置 Winit + WGPU 播放器。reel-render 的 wgpu feature 单独提供 gpu::WgpuBackend，不依赖 Winit。reel-player/platform 定义 PlatformHost，reel-render 定义 RenderBackend，reel-media 定义 AudioOutput；AssetIo、Clock、WorkExecutor 在 reel-core 中定义。player 组装适配器并转换输入，engine 与 reel-ui 不反向依赖 player。UI 控件与布局同窗口外壳和 GPU 后端分开，以依赖检查和限定构建验证隔离。
原生窗口或固定显示目标的创建、表面来源协商与销毁留在具体 player 组装代码，不进入公共 trait。可借用或包装原始句柄，但有效期必须覆盖表面使用期；先停止提交并释放表面，再销毁显示目标。winit 事件、wgpu 设备与纹理、Vita 原生句柄都不得进入会话、RenderFrame 或 DSL。每个后端负责上传、格式转换、离屏目标、设备重建和完成后回收，不能仅替换最终绘制函数。
Cargo features 决定编入哪些适配器，运行参数只在已编入且能力匹配的组合中选择。游戏包声明必需能力；缺失时报错，有声明的低质量后备才可降级。reel-ui 与默认外壳、Kira 与默认输出的目标依赖都要检查，关闭桌面 feature 后不能仍被传递依赖强制引入。不建设动态插件系统或通用平台框架。
以下 Rust 签名规定模块边界，自定义类型是引擎自有数据；不统一要求 Send + Sync，只约束实际跨线程的数据。下文给出类型的最低字段与结果语义，D01 完成对应定义与契约测试。
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
以下字段是接口的最低语义要求，D01 将其落实为 Rust 定义与测试；字段名称可随编码规范调整，含义、单位、所有权与终态规则不得由各代理自行重定。
- 身份：各类请求 ID、FrameId、表面代数使用相互独立的 u64 包装类型。Scope 为 Session(session_id, epoch, owner_id) 或 Process；保存写盘使用 Process。MonoTime 是单调纳秒 u64，UnixMillis 是可缺失的 UTC 毫秒 i64，均不含本机指针。
- Rejected<T> 保存原始 request:T 与 reason。reason 至少区分 Busy、Unsupported、InvalidRequest、Unavailable、OutOfMemory 和服务故障，附拥有型诊断文字；只有 Busy/Unavailable 等明确可重试原因允许原请求重试。拒绝不产生已接受 ID 或业务副作用。已接受请求的终态为 Completed(payload)、Cancelled、Failed(error)，结果记录包含请求 ID 与 Scope。Cancelled 用于释放与记账，不重新唤醒已取消业务。
- QueueLimits 含 max_requests:u32、max_bytes:u64；BudgetTicket 含预算类别和 bytes:u64，随拥有的数据转移，最终释放一次。接受请求时同时保障完成结果的队列容量。工作预算分别预留输入、最大输出、临时工作区和完成记录；解码大小尚未知时先有界探测头部，再在大分配前取得输出额度。CPU 输入、解码结果与 GPU 资源各自持有票据，上传过程中的重叠驻留分别计量。WorkBudget 含 max_units:u32、max_bytes:u64、deadline:Option<MonoTime>；WorkProgress 报告实际消耗与是否仍有工作。
- PlatformCaps 含指针、触摸、手柄、文本输入、剪贴板、可访问性、挂起恢复、等待及唤醒支持位。TextInputState 含启用值和逻辑坐标的插入点矩形。输入事件带输入序号、设备身份和事件时间；尺寸事件同时携带物理像素、逻辑尺寸和缩放系数。
- SurfaceInfo 含 physical_size:[u32;2]、scale_factor、surface_epoch、color_encoding 与 alpha_mode；零尺寸代表暂不能呈现。RenderCaps 含支持格式/效果集合、纹理与离屏尺寸上限和上传队列限制。UploadBatch 拥有资源版本、尺寸/stride/格式、像素或网格数据及预算票据；所有乘法和长度校验先于接受。
- PresentOutcome 为 PresentAccepted(FrameId)、Skipped(reason)、SurfaceUnavailable(reason)。RenderEvent 至少有 UploadFinished(id,result)、ResourcesRetired(versions) 和 DeviceLost(error)；可选的实际呈现反馈另带 FrameId 与时间，不能替代资源回收通知。返回的 FrameId 必须对应本次提交帧。
- ReadRequest 含资源身份与可选固定版本、offset:u64、len:u32、优先级、Scope 和预算票据；读取结果拥有 bytes 并返回确定版本，明确 eof，长度不得超过请求。SaveWrite 含槽位、提交序号、完整槽位数据、最低持久提交级别与预算票据。IoCaps 声明范围读取、读取/保存队列限制及可提供的提交级别；只保证可见替换、不保证持久性的实现不得报告持久保存成功。
- WorkRequest 含 kind、拥有型输入、Scope、优先级及预算票据；kind 限于已注册的解码、编码、校验操作。WorkCaps 含 Threaded/Cooperative、支持种类和队列限制；不支持的 kind 在接受前拒绝。处理进度不算终态。
## 平台与时间
PlatformHost 提供命令，原生事件循环转换输入后驱动公共会话与 Presentation，不要求平台提供阻塞的 pump_events。PointerEvent、TextEvent 和 WindowEvent 沿用 reel-ui 的共同数据；原生生命周期驱动 Presentation、RenderBackend 和 StoryRuntime 的挂起/恢复。Presentation 只接受严格递增的表面代数。组装方在恢复时先失效旧输入，再配置后端，成功后恢复剧情时钟；VM 截止时间不扣除挂起区间。剪贴板、组合文本、系统可访问性、触摸/手柄原生适配属于后续独立服务，当前没有空实现声称支持。后台通知复用 reel-core::WakeSignal，实时音频的通知路径仍须单独验证；不把通用平台回调直接作为实时保证。
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
作者图片与字体共用 AssetIo/WorkExecutor 装配。开发资源以 AssetSource（资源名与可选固定版本）请求；describe 返回长度与可用版本，read 返回本次不可变字节的确定 ContentVersion。没有预先版本索引的目录无需启动扫描；默认目录适配器在后台整文件读取时确定版本，并声明不支持范围读取。发布/内存适配器可提前提供固定版本和范围读取。解码输入使用 Arc<OwnedBytes>，探测与解码共享同一份输入及预算，先后台探测尺寸，再取得输出/临时额度。普通后台队列复用同一终态和取消合同，不由图片节点创建线程；字体准备期间保持旧画面，完成后统一发布设置与舞台。

默认窗口的资源验收包含实际 DPI 和裁剪所需临时目标。WGPU 准备帧前按完整组、裁剪、遮罩与转场计算同时存活的目标峰值，再在既有质量档位中选择能容纳整帧的档位；不以单张纹理能容纳就假定整帧可行。只有裁剪、没有独立遮罩的组，将覆盖率、组透明度和源图直接合成到父目标，不再构造中转纹理；保持原来的预乘色与 source-over 结果。此路径纳入 reel-render 的像素验收，沿用帧接口和预算，不增加作者参数。

Aside 的批量容器操作由 VM 自身执行，不经 Reel 宿主。数组插入、删除、扩缩、反转和扩展共享可恢复的工作副本与一次提交路径；参数名绑定在修改前完成。提交只交换受管理的存储，保持对象身份和预算总量，并使增量 GC 重新扫描移动后的边；取消由现有帧根清理处理。不得为了批量操作引入一次性整数组复制、扫描或释放的主线程停顿。

Aside 文本基础方法复用 Rust 标准库的 UTF-8 解码与标量迭代；分片只保留至多一个未完整标量和位置，不因字符串总长申请临时整串副本。切片与连接使用受计量的私有字节缓冲，完成时原地封为 string。文本连接不反复拼接已生成前缀；来源集合版本、取消与错误沿用 VM 所有权路径。

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

节点公开只读可观察 `hover`、`pressed`、`focused`、`disabled`。hover/focused 汇总节点及后代；pressed 汇总指针捕获，并包含按钮自身的键盘/手柄确认按住状态。disabled 反映继承祖先禁用后的有效值。隐藏、禁用、失焦及退休清除交互中的状态；状态在调度业务回调前发布，展示节点可以直接 bind，无需回调或逐帧脚本。`decorated: false` 关闭按钮默认焦点框、开关默认指标及其留白，保留交互和普通 children；作者用文字、图片、背景、变换及绑定组合外观。仅由交互状态驱动的外观更新保留按钮的在途激活；业务内容变化仍要求接受新画面，新手势不得使用未接受的内容。构造参数 decorated 默认 true，当前节点 native ABI 为 3，B01 不变。

节点内容与交互行为分开。图片可以具备按钮行为，容器可以整体变换；按钮外观读取 hover、pressed、focused、disabled 等状态，滑条提供轨道与滑块槽位。作者组件与内建组件使用同一舞台能力，具体调用形式见 DSL 文档。节点移除注销绑定、订阅并取消所属任务与活动；隐藏保留身份，脱离必须由显式所有者接管。visible=false 隐藏子树并退出命中与焦点；enabled=false 保留画面并禁用子树交互。两者使捕获失效时统一释放捕获；opacity=0 不自动禁用输入。
仿射变换、裁剪和命中使用同一几何，命中经逆变换计算。非线性视觉变形默认采用作者指定的逻辑命中区，扩展映射另行提供。适配必须验证变换后控件的捕获、焦点与命中一致性。
便携的 RenderFrame 与绘制命令定义于 reel-ui::frame，帧资源由 reel-ui::resources 管理，基础资源身份定义于 reel-core；UI 从不反向依赖 render。Mesh、Material 等扩展类型按既定功能范围逐步实现。RenderFrame 包含帧号、画布与输出尺寸、资源引用以及有层次的绘制计划。计划保留 BeginGroup、EndGroup、裁剪、组透明度和效果参数。组内绘制连续，子节点排序受组边界约束；跨组覆盖由显式 overlay 容器表达。组透明度对合成结果应用一次，效果外扩计入离屏范围，再遵守对应裁剪语义。
普通容器直接合成，遮罩与后处理按需使用离屏目标。先实现相邻兼容绘制合批、排版与字形缓存、离屏目标复用；逐字显示复用完整文本排版，只改变显现范围。属性变化区分布局、内容、合成和命中；淡化不使排版失效，资源版本变化传播到依赖缓存。GPU 资源在实际使用结束后释放。低端模式可降低效果分辨率和采样数，逻辑坐标保持一致。

可写绑定的标量写入与 tween 均沿引用链写入最终源 PropertyId，保留绑定。run 解析并保留真实源身份；被动投影只读取结果，外部源写入按统一规则停止根活动。只读派生属性拒绝直接写入，可用独立 override 层作演出。binding replacement/unbind 先停止依赖旧链的活动；unbind 固定当前基础值。override token 拥有独立属性存储，也遵循读写能力与保留规则。

属性连接由独立 VM 的宿主属性存储维护。宿主显式登记可观察字段及权限；前向引用是 GC 强边，反向订阅是弱边。属性传播只遍历受影响的连接，校验期间读取同一候选图，成功后更新缓存与字段版本；失败保留全部旧状态。绑定创建、替换、解绑、来源退休均维护订阅。写入与动画使用最终属性身份，节点字段继续作为舞台投影的增量版本来源。传播工作缓存复用并计入 VM 内存预算，空闲无订阅轮询。节点构造接受 PropertyRef 的 ABI 为 3；B01 使用现有 PropertyRef 指令，不引入平台依赖。

## 六 作用域 样式与状态

`clock(interval: 1, owner: null)` 启动可观察的周期活动，复用活动图、期限索引、暂停/恢复/停止、等待和所有者退休。只读数值 `ticks` 与 `elapsed` 从 0 开始，仅在周期边界发布完整周期数与秒数；interval 必须有限且大于 0。长时钟步长合并为最新周期，不逐个补发，也不创建脚本回调。暂停不计时，停止后保留最后发布值；无终点的 clock 不能 finish。owner 规则与 run 相同。新增 native ABI 1，不改变 B01/ASB；参见 examples/author_clock.aside。
scope 是普通宿主 API，通过 VM 的 CallScript/native continuation 在原 Task 中执行 body。每个作用域有 RegionId 和清理记录，管理显式加入的节点、覆盖层、偏移和活动。正常返回/异常同步幂等清理，退场动画由 body 显式等待。作用域被附加 owner 撤销时，VM 只撤销到 RegionId 屏障的区域，宿主先清理，再于外层 scope 调用点交付 ScopeCancelled；外层主剧情仍可处理。硬撤销跳过区域内脚本 finally，普通 return/throw 仍按语言规则展开。
属性覆盖是独立 token，记录目标、字段、优先序和值或绑定。基础属性与绑定继续更新，求有效值时取最后建立的有效覆盖；移除某层只显露剩余层或最新基础值。活动写入明确的基础位置或覆盖 token，effective 值只是解析结果。覆盖字段的直接写入与动画保留遵守统一冲突规则。
s.offset(node) 创建父坐标系中独立的 x/y 加性偏移层；各层相加后进入节点最终变换和命中矩阵，原布局值保留。旋转/缩放采用有序矩阵和组节点，避免把非交换变换伪装成加法。偏移与覆盖均可进入快照，失效时释放命中及绘制依赖。s.run 始终归 scope；run 默认归当前 scope，否则归 Task 的 owner，显式 owner 表示明确延长生存范围。目标失效仍按根活动取消规则处理。
样式复用由普通组件函数、记录与参数完成。组件默认值、变体、显式参数按序合并；显式缺失与 null/false/0 区分，scope 覆盖最高。运行期样式通过可观察属性绑定，布局/绘制失效分类保持一致。引擎提供通用属性和控件槽位，作者决定视觉风格。
model(defaults) 复制默认图并保持其中的别名/环，所有可变内嵌数据对象使用观察变更钩子，读取与结构操作记录对应依赖；后续会话对象赋值保留共享身份并纳入观察，别名修改同样通知。原输入默认图保持独立。对象按可达性保活。state(id,defaults) 是会话内的具名状态根，随保存/回滚恢复，新游戏初始化。node.state(key,defaults) 属于具体节点实例的状态槽，在节点 key 与槽 key 同时匹配时重载复用；同名不同实例互相独立。组件销毁后保留的普通脚本引用按正常 GC 规则决定对象是否继续存活。
`model` 的数据图复制由原生 continuation 分片执行，工作对象随 NativeFrame 取消释放，不把复制循环塞进单帧。当前数据范围为 table、array、tuple、字符串、字节串及可变缓冲；可执行对象与宿主资源不属于可复制数据。model table 复用普通 table 身份、方法与属性连接图，新字段可以追加；clear 断开旧字段连接并冻结消费者，旧 PropertyRef 不得指向后来同位置的新字段。后续赋入的数据保持共享身份，嵌套 table 同样可 bind。Reel 在这些同步 continuation 之间保持当前任务，复制不会与其它脚本任务交错。新增 `reel::model` native ABI 1，B01/ASB 不变。

派生绑定复用属性连接图：正向依赖保活来源，反向订阅是弱引用，写入只将相关派生加入去重队列。VM 记录实际成员、索引及集合读取；每次成功计算替换条件依赖。会话在同步脚本段结束与舞台发布之间按 VM 预算排空计算，持续占用原有任务调度边界，避免计算期间其它脚本写入。局部临时对象按计算代数识别，不允许改写外部对象；原生回调只能收紧权限。

persistent(id,defaults) 是显式进程级数据根，跨游戏会话存储；defaults 仅补缺失项，已有 false/null/0 保持原值。新实例默认复合对象复制后归该实例，避免共享可变默认状态。持久数据由独立数据图与适配器保存，禁止把会话 Node/Task/可执行闭包挂入跨局数据图；写入时检查引用路径与寿命。跨局赋值复制到持久数据图，内部嵌套对象通过稳定 GlobalDataId 的代理访问；所有别名修改均验证并标脏，快照记录 GlobalDataId 并重接当前数据。全局设置属于独立服务。
普通运行期 state/persistent/node.state 调用原子补充缺失默认字段，下一语句可见。InitSafe 的 story.register、state、persistent 及组件状态默认补充先登记候选变更，候选读视图可见拟加入字段而活会话不可见，ModuleSet 提交时才一起生效；失败不修改旧注册表或全局进度。全局数据的普通修改立即可观察、后台合并写盘；flush 返回可等待的可靠提交请求。跨结构版本迁移使用版本化纯数据回调，在隔离数据图成功后再替换。

## 七 VM 与会话接口

宿主 Awaitable 复用 Aside 的 native 等待帧，不另建引擎专用 VM 等待系统。共享活动由 adapter 保存一次终态及登记顺序；每个消费者拥有独立 WaitId，取消消费者只解除其订阅。活动目标/owner 的取消由活动自身账本负责，再向仍有效的消费者交付结果。Await 的对象、返回槽、继续位置和故障源都由既有 VM 根持有。此阶段只交付通用宿主协议，Reel run/tween 及其逻辑时钟随后接入。

本次范围：aside-compiler 的 await 降低与源码合同、aside-vm 的可信宿主绑定/NativeFrame/故障图、规范和双语指南。沿用 B01 的 Await 指令和 ASB v1，无新增依赖。验证源码/字节码往返、已完成与未完成等待、共享对象多个消费者、故障定位/原因、受限上下文、微小预算下 GC 与取消、旧完成过滤。每次等待复用一个 native 帧，不扫描其他任务或复制宿主 payload；资源收费使用已有堆、FrameSlots 和 native fault 预算。

宿主函数也是可保存、传递、返回和可选调用的普通函数值。编译器保留可信注册身份，VM 在模块常量根区持有函数对象，重复取值不分配、不增加作者全局变量。动态调用直接进入 NativeFrame，与直接调用共享参数、缺省 Cell、效果和等待处理；不插入脚本转发帧。`text` 可作为组件工厂参数，`narrator` 经变量调用仍产生原来的交互等待，完整源码与 ASB 示例见 [function_values.aside](../examples/function_values.aside)。
会话管理主剧情、UI 回调任务、交互数据和活动所有权。启动与主剧情任务归会话所有，UI 回调归发出事件的节点所有；替换场景结束旧节点及其活动，主剧情任务继续。主剧情同一时刻只有一个交互。game 服务身份稳定，game.dialogue 属于当前会话，settings.music 属于全局设置。发言更新完整文本与显现范围并追加一次历史，再等待推进；文字未显示完时首次推进只完成文字，后续推进才结束等待。choose 将输入普通表复制成引擎拥有的只读可观察选项行，id、key 与 label 均可读，id 是 InteractionId 加 key 的不透明稳定标识，列表用 id 区分连续选择；select 接收带 InteractionId 的 option.id，验证后返回业务 key。入口与剧情 helper 均为普通 fn，剧情元数据按需附加；具体语法见语言与 DSL 文档。
VM 的 run_slice 接收指令、内部工作、分配和计时检查额度，返回 Returned、BudgetYield、HostCall、Waiting、Fault 或 ResourceBlocked。ResourceBlocked 保留帧、根和中间故障阶段；不能把它视为普通可丢弃错误，也不能假定 GC 会释放仍活跃的 catch/展开记录。宿主调用携带任务身份、参数和源码位置，返回 Ready、Pending、CallScript、Continue、Error(NewFault) 或 ForwardFault(FaultId)。Continue 保存 native 算法状态并于同 Task 下一预算片继续，无外部等待。CallScript 保存纯数据 native continuation，退出原生栈后在同一 Task 压入脚本回调帧；回调可按其效果上下文挂起，返回/异常恢复 continuation。Pending 先退出 Rust 调用，再建立显式恢复记录；恢复记录保留操作、可恢复参数、返回目的槽与下一指令位置。禁止把原生调用栈当作可保存续体。同步原生重入 VM 不得跨边界挂起，异步宿主算法使用显式状态机。
SessionEpoch 在新游戏、读档和回滚时更新；任务、等待、节点、回调和活动各用独立代际身份。后台完成结果只有与仍存活的 SessionId/Epoch 及等待匹配时才交付一次。取消、节点销毁和会话替换使旧结果失效。
ActivityHandle 本身实现 Awaitable，支持暂停、继续、停止和到终值完成。stop 保留当前值返回 Stopped；finish 对有限活动执行到终态返回 Completed；无限重复没有完成终点，由 stop 或 owner 结束。终态为 Completed、Stopped、Cancelled、Failed，只提交一次；前两者返回相应结果，后两者在等待位置产生取消或脚本错误。所有者取消任务时直接终止该等待。所有 VM 任务遵循同一可等待对象契约，恢复身份进入快照。主剧情输入权限仍由会话管理，与函数声明形式分离。
演出描述先纯构造，再由 run 在显式所有者下启动；顺序组合不接收已运行句柄。动作激活时读取起值。活动只在动作段激活时取得写入权，新段从当前值接管并停止冲突的旧根活动，暂停活动亦如此。sequence 内重复属性按顺序使用；parallel 各分支的写集合必须互斥。直接给正在由活动写入的属性赋值会停止对应根活动，包含同值赋值，不影响其他属性的活动。显式 stop 使根活动及余下子动作返回 Stopped；任一子动作 Failed 则结束整组并返回错误。节点与效果目标使用有稳定身份的引用，run 校验目标有效；活动保活脚本对象，不延长显式舞台所有权，任一目标销毁使整个根活动 Cancelled，释放全部保留并向仍存活的等待者交付取消错误。所有终态只提交一次，均释放保留。持久活动图包含子活动、进度与等待关系，并提供可序列化恢复描述。描述中不保存闭包，事件回调排入普通任务。接口见 DSL 文档。
已读标记按稳定语句 ID 记录。按住快进由输入保持状态驱动，松开、失焦、进入菜单和读档时清除；自动播放属于会话状态，文字速度和音量属于全局设置。更换 UI 不改变历史。

作者对白与选项使用 `game.dialogue()` / `game.choice()` 稳定服务对象。会话交互变更时发布行组/选项，显现时仅发布当前行的 reveal；已完成行保留到显式清页，历史独立保存。通用 view 容器沿用派生依赖队列与 native continuation 执行组件工厂，成功才替换内容，退休旧子树沿现有节点所有权取消任务。默认播放器界面可分别关闭，作者视图使用相同舞台和已呈现输入协议。新增服务、derive、view native ABI 1；game.configure ABI 2，节点新增 reveal 后构造 ABI 4，B01/ASB 格式不变。

作者阅读服务提供稳定的只读可观察 `game.history()` 列表与 `game.reading()` 状态；`game.set_auto(enabled, delay: null)` 修改自动阅读及全文呈现后的停留秒数（默认 2 秒），`game.pause_reading(paused)` 暂停对白显现与对白/选择输入，普通作者控件继续工作。状态属于会话，UI 更换不重置；新会话默认手动、未暂停。自动阅读仅推进已完整呈现的对白，选项等待玩家操作。失焦、挂起、模态界面、资源未就绪或输入手势期间不累计阅读时间，恢复后重新等待完整停留时间。有限演出与独立音频不因阅读暂停而停止。

历史组提供 `group_id/interaction_id/locale/lines/speaker_name/text`，行提供 `id/speaker_id/speaker_name/content_id/text`。行 ID 与对白模型一致，在同一会话中稳定。历史全文在提交时记录一次；清页和界面替换不删除历史。首次访问按已有历史建立投影，之后只追加新增组，显现更新不扫描历史、不复制全文。数据/控制服务不创建 UI，作者用通用组件组合回看和阅读控件，示例见 [author_reading.aside](../examples/author_reading.aside)。新增 native ABI 1；B01/ASB 格式不变。恢复点、语音重播和已读跳过依赖各自后续服务，不在历史记录内提供占位字段。

`game.set_skip(enabled, unread: false)` 控制快进，`game.reading().skipping/skip_unread` 可观察当前状态/策略，`game.dialogue().read` 表示当前整组在本次提交前已读。默认在未读组和选项处停止；unread: true 允许略过未读对白，仍不替玩家选择。快进沿原控制流执行，略过文字显现和停留，并让主剧情拥有的运行中有限演出按既有 finish 语义到终值；无限、暂停或节点拥有的活动与独立音频保留原控制。自动阅读与快进互斥。暂停阅读、失焦、挂起或出现模态界面清除快进；键盘/手柄保持通过 input 回调在 pressed 启用、released/cancelled 关闭，不添加固定快捷键。input 发起的快进登记节点身份，退休时清除保持状态；普通按钮与主剧情设置的模式属于会话。

已读只在对白组完成推进后登记，取消和仅显现全文不登记。Session 使用按 content_id 索引的 `ReadProgress`，提交时预留空间，完成时移动已有内容 ID，输入热路径不分配。Rust 宿主通过 `Session::new_with_read_progress` 复用记录，`ReadProgress::ids/from_ids` 对接自己的持久存储；没有隐式磁盘访问。已有内容 ID 包含模块版本与源码位置，修改版本保守视为未读。历史的 group/line ID 是会话展示身份，不作为跨会话已读键。新增 native ABI 1。

## 八 资源与音频
资源以稳定 AssetKey 和内容版本管理读取、解码、上传与缓存。取消预取、场景退出与过期结果都释放所属额度。图片、字形、视频队列、GPU 纹理和临时目标分别记账；缓存只保留可逐出的额外引用。直接资源引用与作者预加载共同提供预取提示，动态分支确定后调整集合。

wgpu 图片缓存按完整 ResourceHandle 复用已提交的纹理；准备失败或未提交的上传不发布到缓存。纹理驻留票据随共享图片保留，转换和 staging 票据随原提交完成释放；后续帧只登记其使用的驻留额度。额度不足时逐出没有准备中或在途使用者的缓存，资源退休清除缓存引用；在途资源等 GPU 完成再释放。设备代数不共享缓存，其他 HAL 后端保持各自资源策略。
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
本地化：`tr(id, fallback, params: null)` 按当前语言返回普通 string；在 `derive` 内调用会订阅语言目录版本，沿用同一套响应系统。`localization.load(locale, source)` 通过宿主资源服务异步读取 UTF-8 Fluent 文件，当前任务等待完整解析后原子替换该语言目录；错误可捕获，旧目录保留，任务取消不发布结果。`localization.set_locale(locale)` 切换语言，`localization.locale()` 可观察读取当前语言；查找依次使用精确语言、基础语言和原文。未传 params 时原文是字面文本；传入 table 时原文使用 Fluent 模板，命名参数只接受 string/int/float。译文始终使用 Fluent，模板解析与复数规则复用固定版本实现。参数不作为模板再次解析。UI 通过 derive 即时更新；已求值的 string、已提交对白/选择、聊天输入和历史保持原值，作者可以保留 ID/原文/参数并在展示时重新解析。此接口替代隐式 LocalText 值，避免普通字符串和响应值形成第二套求值规则。目录后台解析一次，空闲帧不轮询文本；载入队列沿用资源容量限制，VM 不进行文件 IO。新增 native ABI 1，Aside/B01 不变。自动 ID、reel loc 提取与资源翻译仍属后续工具能力。
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
ASB 1.0 的模块元数据使用 schema_version=3：保存全部源码模块、各自来源/声明/绑定、完整初始化顺序、实际启动函数；B01 与 native ABI 保持 v1。导入和重导出共享原绑定，运行时使用一次验证后缓存的模块索引，不扫描模块表。源码文件与模块身份分开保存，构建产物直接执行已链接的完整模块图。

发布产物分为字节码包 .asb 与资源包 .rpk，随目标平台播放器和 release.json 清单发布。一个游戏可以有多个代码模块包与多个资源分包；脚本修订和图片/音频更新独立构建。此分层借鉴 YU-RIS 的脚本二进制与资源归档组织，格式由 Reel 自行定义。
.asb 保存入口、模块/函数/导入导出、常量、稳定内容与恢复标识、语言/字节码/宿主 ABI 版本和所需能力。源码映射可作为独立调试包，去掉源码后仍运行完整字节码语义。.rpk 保存 AssetKey、内容版本、目录索引、块表、编码/压缩信息和数据；索引与 offset/len 使用固定宽度，range read 支持流式音视频。已压缩媒体可原样储存，通用数据按独立 zstd frame 压缩，避免为一个 seek 解压整包。

### ASB v1 代码容器

共享容器由 reel-package 定义，只依赖便携 B01 格式与必要序列化/摘要库，不依赖编译器、UI 或 GPU；编译器 HIR 到便携元数据的转换和文件写入由 reel-cli 完成。B01 继续是代码、模块、函数、导入导出、常量、ABI 与能力表的唯一数据来源，容器不复制另一套同义表。

所有整数小端、偏移为文件绝对字节位置。固定头 72 字节：magic[8]=REELASB 加零字节、major:u16=1、minor:u16=0、flags:u32、total_len:u64、index_offset:u64、index_len:u64、index_blake3[32]。索引项固定 56 字节：kind:u32、flags:u32、offset:u64、len:u64、blake3[32]。v1 必需且唯一的区段为 kind=1 的既有 B01 字节流和 kind=2 的版本化 UTF-8 JSON 元数据；未定义的必需语义拒绝，已知 flags 外的位不默许。读入先检查总长度、索引长度可整除记录宽度、数量/字节/工作预算，以及 checked offset+len、区段重复和与头/索引的重叠，再验证摘要及内容。不会用磁盘长度直接进行无界分配。摘要检测完整性，不证明来源或授予宿主能力。

schema_version=3 元数据保存全部源码模块的规范路径、源码版本/摘要/范围、source symbol 范围和函数/绑定/global 映射，以及工程初始化顺序、initializer、可空 game_entry。记录使用稳定 DTO 与精确整数。函数映射包含作者函数；编译器生成的默认 thunk 和 initializer 由已验证 B01 保存。读取端交叉验证模块/函数归属、范围、映射键唯一性、源码 UTF-8 边界及精确 B01 摘要；源槽与效果摘要和 B01 验证、可信 native 注册、运行期效果检查各有明确职责。JSON 输入受字节、深度、字符串/记录和工作量预算约束，重复字段拒绝。

ProjectCompilation.initializer 调用完整源码图的模块初始化器。build/dev 默认选择 main::main；--entry 可选择其他普通模块级、无捕获、无必需实参的函数，私有函数同样可作启动入口。具名函数经 HIR 输出映射到 link-time global 与 B01 FunctionId，重导出入口解析到原声明。缺少有效入口时在对应源码位置报错；底层容器 API 可显式生成 game_entry=null 的模块产物。reader 验证实际入口为 Ordinary，并核对初始化器与完整顺序。消费者完成工程 initializer 后启动入口；CLI 编译与验证只构建产物。源码模块、固定版本依赖和已注册 Reel native 通过同一个链接程序执行。

命令为 reel check <source> [--format human|json] 与 reel build <source> --output <file.asb> [--entry main::name] [--overwrite] [--format human|json]。build 在目标同目录完成唯一暂存文件，编码与验证成功后以原子不覆盖操作发布；只有显式 --overwrite 允许原子替换。先检查 exists 再 rename 不构成不覆盖保证；失败保留已有目标且只清理本次暂存。原子可见与断电持久性分别说明，不能把普通成功当持久存档承诺。

静态检查读取上限目前为 8 MiB，后续编译/B01 仍各自执行更紧的预算；例如当前 B01 源码字符串限制为 1 MiB。诊断 JSON 保留字节范围及零起始 byte/scalar/UTF-16 坐标，人读输出用实际路径及可读位置。逻辑模块标识（如 main）与物理源码路径独立；B01 SourceFile.path 保存调用方提供的源码路径，dev 与 ASB run 的故障使用包内来源和源码定位，不把逻辑模块名当文件名。完整 .asb 的 BLAKE3 用于产物身份；占位 source_version="source" 或 content_version=1 不代表唯一内容。当前 B01 包含源码与映射，不能称作已剥离源码的发行包。.rpk 与 release.json 仍待实现；源码及 .asb 已接入真实剧情窗口，物理 GPU 的验证范围另行记录。

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
普通 string 和 tr 的翻译结果保持字面含义。rich(tr(...)) 显式解析最终翻译中的标记，玩家输入等字面参数须先 rich_escape；模板参数不会自动取得样式权限。rich 返回不可变宿主值，UI、对白与历史共享 reel-core::text::Text；普通字符串仍可直接使用。
文本服务先形成逻辑字素序列，再完成 shaping/cluster 映射；revealed_count 指逻辑字素边界，完整 cluster 按该映射显示，ruby/内嵌对象的揭示作为声明的原子单位。当前活动对白组保持提交时的文本、语言和语音，下一次 tr 使用新语言；累积显示的旧行保持原文。UI 用 derive 重新解析作者保留的翻译描述，普通历史字符串保持原值。显式刷新对白与语音重播仍是后续能力。

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

作者数值控件
`slider(value: 0, min: 0, max: 1, step: null, on_change: null, ...)` 与 `toggle(label: null, value: false, on_change: null, children: [], ...)` 使用同一 Stage 和共同外观属性。slider 复用 reel-ui Slider，toggle 复用 ButtonBehavior；开关可带标签和自定义子树。slider 的 min/max/value 必须有限，min < max 且跨度有限，value 在闭区间内；step 为 null 或有限正数。步进以 min 为锚点，两端始终可达。脚本可以设置任意区间内值；step 只约束用户输入。范围属性写入先检查完整当前状态，失败不修改原属性。
控件动作先更新唯一权威 `node.value`，再调度 `on_change` 的节点任务，事件包含 target 和此次 value。直接脚本赋值静默投影，不触发回调。闭包可等待并沿用节点退休取消；省略回调仍保存控件值。slider 默认水平轨道，color 控制活动轨道/滑块，background 仍是共同节点背景；toggle 以状态指示和可选标签呈现，children 按 stack 叠放。
Slider 统一绘制与指针逆映射的滑块中心端点，退化几何没有指针值。呈现几何记录控件交互配置版本；range/step 变化取消旧捕获，在对应帧被接受前阻止输入，value、尺寸和变换变化保留配置版本。指针使用接受帧的几何，键盘/AccessKit 从当前值增减。连续控件事件不以每次 value 更新的整个会话投影版本重置手势；已退休身份、当前可交互状态与范围仍在提交时检查。没有输入时不启动 VM 轮询任务。宿主可用 `SessionUi::handle_access` / `AsideSessionUi::handle_access` 投递 AccessKit ActionRequest，通过共同呈现几何、模态路由和回调入口；原生屏幕阅读器适配不由此 API 自动提供。

源码按职责组织为 reel-ui 内部模块，控件基础、树、呈现桥接不再拆成独立包。保留原始许可和必要版权/来源说明，普通外部依赖继续锁定引用；验证以当前行为、单一类型来源和依赖边界为依据，不做上游目录或补丁重放门槛。生产验收覆盖首帧/清空、缺失子查询、路径变更、单次终止、绘制组跨片段、外部内容作用域，以及图片滑条与业务绑定的同一数值/几何。

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

基底层与其他层使用一致的 layer-local-to-window 约定，绘制与 PresentedGeometryView 使用同次变换。内容坐标包含 border-box 的相应平移，clip 使用明确的 border-box 坐标。有限但奇异的变换可以保留视觉几何，输入逆映射返回空；导出转换使用显式上下文和前向变换，避免强制求逆。前向累计使用 UI 的完整 compose_local_transform，包含 layout origin 和 scroll translation；几何记录中单独的 transform 字段仅是其组成部分。非有限变换在变更提交/导出时给出定位错误。消除已知基底层变换偏差并通过像素/命中对应检查后，替换 U02 的临时 UnsupportedRootTransform 边界。

paint/append 边界另存连续命令的实际绘制者范围，普通 Inline 子控件的命令和定位错误不能以片段边界 owner 代替；范围游标在已计费的命令提交/片段推进时只访问相邻范围，初始化以固定一次索引读取得首个 owner；下一命令完成映射预付款后发生错误时仍报告该命令的实际 owner，诊断不做未计费扫描或延后查找，payload 暂停继续保留相同来源。生成的 clip、调试框和检查器绘制也保存实际 owner。调试 bounding_box 已在窗口坐标中，使用 identity 片段；baseline 前向累计到窗口坐标，跨片段保持打开的作用域，不对奇异变换求逆。

组明确保存效果坐标空间、效果参数和保守 ink bounds。imaging Group 本身缺少整体效果变换，因此 paint/append 边界为组保存保证存在的 owner 与效果空间元数据；组的来源不能靠某个子绘制或可选诊断上下文猜测。bounds 包括笔画、字形外延、抗锯齿与滤镜扩张，布局盒仅用于布局。mask 自己的记录流、变换、alpha/亮度模式和采样规则随组保留。

### 默认后端方向

默认使用 wgpu 的常规几何与合成管线：图片四边形、缓存路径网格和字形图集四边形相邻兼容时合批。路径细分复用 lyon_tessellation，字形栅格化复用 Swash scale；保留现有 Parley 整形结果。候选依赖版本分别为 1.0.22、0.2.10，实施时锁定并检查实际依赖统一、许可证与目标构建。Swash 的 shaping 路径不进入此适配器。

颜色在合成前转为线性并预乘 alpha，输出到表面时按 SurfaceInfo 编码一次。图片准备按源编码/alpha 描述转换；纹理过滤采用线性预乘数据。渐变保留声明的插值规则，产生的采样颜色进入同一线性合成约定。灰度字形覆盖率与颜色分开，彩色字体图像按自己的编码转换。

普通绘制直接进入当前目标；需要独立组结果、mask 或 transition 时才使用离屏目标。EndGroup 对完整组结果应用一次 mask 与组透明度。alpha mask 使用覆盖率；亮度 mask 的覆盖率为 clamp(dot(linear_premultiplied_rgb, [0.2126, 0.7152, 0.0722]), 0, 1)，alpha 已包含在预乘值中。crossfade 对两份冻结输入的线性预乘结果按进度混合，遮罩转场沿用相同覆盖率与组机制。矩形裁剪可优化为 scissor，变换/path clip 使用真正的几何覆盖或 stencil，并保持跨片段、外部内容和离屏组的继承关系。

CPU 实色、文字与图像转换复用 Peniko 导出的 color 类型，按输入的 sRGB/linear 和 straight/premultiplied 标记转换为统一线性预乘值；独立公式仅作为测试 oracle。虚线由 Kurbo 处理偏移、奇数项模式、曲线长度和闭合接缝，再流式交给 Lyon 细分，不维护另一套虚线算法或完整中间路径。全零虚线模式按实线处理。Bevel/Round 不受无关 miter 参数限制；Miter 的非负小于 1 限制等价于 Bevel，不因底层断言拒绝合法帧。网格仍按实际 transient 预算与设备表示能力计量；Kurbo/Lyon 内部临时存储另属依赖工作空间。

GPU clip/group/mask 执行使用按实际存储计账的显式工作栈，不设置 32 或 64 层嵌套门槛；目标、资源与临时存储继续归现有真实预算管理。

设备建立时确定并公开精度、采样数与效果能力。桌面质量档以 RGBA16Float 中间目标和 4x MSAA 为首选；受限档显式使用线性 RGBA8 与 1x 采样，字形覆盖继续保留。具体选择由设备能力与已配置内存档共同决定，创建后保持稳定；变更质量档触发缓存/目标版本更新。桌面离屏起始池为 128 MiB，与普通资源驻留分别限制并汇总峰值。1080p RGBA16Float 的 4x 附件加 resolve 已占 82,944,000 字节，深度、mask 与在途重叠另计；64 MiB 等更小配置在设备/尺寸协商时选择明确的受限档，或报告所请求质量的最小资源量。目标池按实际格式、sample count、resolve、深度/覆盖和同时在途数量预先计量，空间不足时保持既有画面并报告资源需求。质量和内存默认值最终以同一基础场景的实际目标设备数据确认。

Vello 0.8 的编码/渐变与 blend 路径采用 sRGB，imaging_vello 0.0.1 对 alpha mask/filter 另有缺失；因此它不是本合同的默认生产光栅器。CPU replay 可作结构与几何参考，线性颜色期望另用明确公式验证。

设备适配实施合同见 [R02-INTERFACE](../crates/reel-render/R02-INTERFACE.md)；该文档区分 device generation 与 SurfaceEpoch、实际队列完成和表面接受，以及独立目标/资源/临时预算。实现与设备验证状态以 R02 报告为准。

WGPU 适配器使用锁定的 wgpu 30.0.1。设备丢失保留后端提供的 reason/message；已丢失设备上的帧获取、丢弃与呈现统一进入设备恢复，不将清理失败变成进程 panic。帧准备先于交换链获取，提交后仍检查设备错误，只有成功呈现才接受输入几何。恢复只替换 GPU、表面与其缓存，保留剧情 VM 和 CPU 资源；旧设备的 prepared 不能提交到新设备。此修复范围为 reel-render 与 reel-player，无脚本/ASB 接口变化；验证真实窗口持帧期间设备销毁、恢复后的呈现与剧情状态，以及既有 GPU 渲染与预算生命周期。

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


## Owner 撤销与执行器整合边界

宿主协调完整 owner 撤销。先将 owner 标为 Revoked，禁止新增任务、根、等待注册和过期完成交付；Vm::cancel(owner) 只取消匹配任务并使硬取消票据失效，后续按预算清理任务。Idle 仅表示没有匹配 Task，不能解释为宿主根或适配器已释放。宿主生命周期记录在 owner 创建时预留有界退休容量，撤销时保存 cursor=0 与 end=heap.root_slots()，每个主线程预算 turn 推进 release_owner_step，传入 slots=min(剩余 work, end-cursor)，该 API 不直接接收 end，以 cursor>=end 判定扫描完成；按游标实际增量计费，空洞同样消耗工作。没有 Task 也必须推进根扫描。该扫描仅释放单根，宿主持有的非任务 RootRange 需独立有界释放游标，不能遗漏。任务清理、根扫描和外部资源释放全部完成才进入 Retired，随后对象由预算 GC 回收；宿主根记录同步失效，避免重复释放或继续使用旧 FaultRoot。

PR #49/#54 的执行器与集合增量已通过 Windows 原生执行检查；源码贯通测试实际执行普通调用/缺省/闭包、异常/finally、索引/成员、可选链和 spread/rest，覆盖微小预算与交错 GC。PR #55 接通 narrator/choose 的真实等待、原 key 根交付、取消及播放器终态。property-bag、delegate/hooks、完整模块/类和快照保持未实现边界；平台设备和性能不由 CPU 契约检查推导。

## S01 Rust 会话宿主实现状态

reel-engine 管理 Session/Epoch/owner、唯一主交互、完整文本与字素揭示、稳定选项、过期和重复输入过滤、历史及终态一次交付。Session::new(SessionId, Epoch) 不接受人为行数、选项数、历史组数、owner 数量或文本长度上限。所有会话存储按实际内容使用可失败的内存申请，历史完整保留，不在容量阈值处截断或逐出。DialogueLineId 的进程内索引使用 usize；owner 的完整身份仍单调生成，只有完成退休后才可复用其存储槽。提交前完成文本复制、字素索引、历史/owner 容量申请及选择 key 去重，失败不发布半个交互或消耗身份；选择去重使用借用原 key 的哈希集合。已接受交互的揭示、输入和终态交付仍不分配。会话投影按实际内容高度布局并滚动；滚动后须等待新几何被呈现接受，旧 Down/Up 不能选择新位置。

owner 退休记录在创建时预留，单根游标、任务和外部资源分别设完成门；游标按实际推进含空洞计费。真实适配器仍需调用 Heap/VM 清理，非任务 RootRange 独立释放；纯 Rust 零义务取消不能代替 VM owner teardown。只有被呈现接受的投影才能路由点击，Down/Up 绑定同一投影；领取 Resume 后仍需在实际交付时复核存活 owner 和真实等待身份。

S01 的历史验证范围是 Rust 模型、真实 UI/window、字体遮罩和软件 GPU 读回，当时未执行 Aside 或 VM。后续 Aside 会话适配和作者舞台已接通可信宿主、真实等待及源码/ASB 窗口入口；作者音频已通过 AudioBackend 接入媒体输出，快照恢复仍未接通。

PR #54 的宿主值读取使用公开 sequence_len/get 与 table_own_string_lookup/step。own-string 游标按条目、字节和完成工作计费，区分缺失与 null，不调用 delegate/hook；调用方用真实 native ticket 或显式 root 保活 table，游标本身不持根，跨轮检查存活与完整内容版本。返回 Value 跨借用期保存仍需显式 root。原始 alloc_record(Table) 存储 schema 不是语言 table，适配器不得猜布局。线性查找复杂度和同步常量加载限制见 VM README；owner/RootRange/外部资源的有界退休责任不变。

## Aside 会话适配（PR #55）

共享的宿主 ABI v1 描述同时生成编译器 prelude/linkage 与运行时签名验证，包内声明不能授权自身能力。真实模块初始化器先于模块级普通入口运行；私有入口按精确 HIR SymbolId 映射输出函数，不通过尚未初始化的 global 猜测。当前 narrator 与 choose 仅属于主剧情 TaskId，尚无一般 UI 回调任务。

交互先暂存并验证配置的会话资源额度；VM 接受 Pending 后才提交可见 Session 状态。实际恢复时复核 interaction、owner、task 和完整 wait 身份；选择的原始 key 保持显式根到交付。owner 撤销分别推进冻结高水位的单根扫描与任务清理，空洞也计费；无任务不等于无根。GC 经过 VM provider。

host_work 仅计选择查找工作，文本复制及发布同步，读取按块推进且不另设总文字长度门。作者文本仍须符合 VM 的实际堆内存预算；会话复制通过可失败的内存申请处理真实分配失败。内容身份由模块路径、版本、源区间或函数/PC 后备组成，不宣称是本地化或跨保存稳定身份。

PR #55 的 Windows 原生 debug/release 验证覆盖真实执行、取消、GC 与终态；性能与设备仍需各自验收。无窗口入口 cargo run --locked -p reel-engine --example story_contract 保留。当前作者舞台已连接 text/image/row/column/stack、children 组合、属性更新、局部子树编辑和有限动画，源码 dev 与 ASB run 共用窗口宿主；命令及边界见本文开头和作者指南。角色、事件/绑定、媒体等待、资源包与快照继续沿同一宿主/呈现边界实现。
## 作者音频活动（play/volume ABI v2，其余 v1）

Windows CPAL/WASAPI 输出协商客户端 Mono/Stereo，使用共享音频引擎的声道矩阵转换；设备默认多声道格式不再被单/双声道筛选排除。其他后端仅公布其已知有序布局，不根据未知原生声道数量猜测扬声器位置。

`audio.play(source, looping:false, volume:1, owner:null, group:"effects", pan:0, fade_in:0)` 立即返回可等待活动，后台完成读取与预解码后开始播放。`volume` 是非负线性增益，0 静音；`audio.volume(activity, value, duration:0)` 更新增益。`pause`、`resume`、`stop` 和 `await` 与演出活动共用作者入口。await 返回 Completed 或 Stopped；读取、解码、输出失败抛出错误，重复等待保留相同终态。Completed 表示混音器消费完内容，不承诺声卡已经播放完。

缺省 owner 是当前回调节点，主剧情中是会话；节点退休取消声音与尚未结束的加载。取消一个等待者不取消共享播放。会话关闭先撤销等待，再停止输出、取消加载及活动，持续回收直到实际 PCM 和后台请求释放。挂起保留 mixer，恢复复用；输出失败只结算一次，不自动伪装恢复成功。

核心通过 AudioBackend 接口接入媒体，不持有平台句柄。播放器将共享 AssetIo 读取与独立解码容量、AudioActivityService 及 AudioOutput 组合。预解码 PCM 按实际容量预留与计账，不设固定音长上限；每包解码检查取消与预算。长轨流式、seek 和设备重建继续遵循上文完整设计。音频控制维护使用 10 ms 后备调度，仅有未完成播放/回收时启用，不请求布局或绘制。

### 混音与音频过渡

AudioGroup 位于 reel-core 的平台无关契约；media 与 engine 使用同一枚举。播放器将 music/voice/effects/ui 路由到四个 Kira 子轨道，主轨道负责 master 增益。每声增益、分组增益与主增益相乘，组设置影响已有与后续播放。子轨道只预留声音槽和内部缓冲，不复制 PCM；每组槽容量为 max_sounds，总播放准入仍共享 max_sounds。四组控制句柄随 manager 一起移交原后台退休服务，按轮转游标和统一配额取走已退休声音。

作者 play 接收 group、pan、fade_in；volume/group_volume/pan 的 duration 和 audio.stop 的 fade 使用非负秒。Kira 在音频消费时钟推进过渡，命令通过现有合并槽传递，无逐帧脚本任务。声音暂停保留播放位置，音量/声像包络继续推进，输出挂起冻结全部包络；淡出停止期间再暂停立即停止；淡出停止只在混音器停止后结算 Stopped，加载中或已暂停则立即停止，owner 取消及会话关闭不等待淡出。新音量/声像命令从当前值覆盖前一个过渡；加载中变更只更新初始值。音量按分贝插值，静音端点 -60 dB，声像线性插值。实时回调不执行分配、析构 PCM、文件 IO 或控制线程等待。

## Rust 音频活动层（PR #53，限定 Rust 范围已验收）

reel-media 的 AudioActivityService 在 Kira 之上保存有界活动账本。请求拥有固定 ResourceVersion、注册 AudioOwner（包含完整 Scope）、MediaEpoch、分组元数据及 DecodedSound；原生句柄不跨模块暴露。接纳前同时保留播放槽和终态槽，拒绝返还原请求，终态保留槽直到排出。MediaId 在服务实例内不重复；替换实例时由宿主提供新的 MediaEpoch。当前分组仅为元数据，循环取自输入声音配置。

DecodedSound 不设置独立 PCM 帧数或时长上限；调用方的真实 BudgetTicket 覆盖输入 capacity 与转换存储，并随 Sound 整个生命周期保留至退休。回收量不从固定 64 MiB 假定推导 PCM 上界。

Completed 表示通过 Playback 观察到内容消费结束，不是设备实际呈现，也不表示 PCM 已释放。Stopped 表示停止命令的逻辑结算；供给停止仍由 AudioOutput 的 fence/stop 协议保证，物理 PCM 回收仍经后台服务和预算票据。此 Rust 终态尚未接到作者的媒体等待；完整媒体完成继续遵守前文设备时钟与时间线合同，不能直接把低层 Completed 当作可听终点。

撤销 owner 时，宿主先撤销业务等待并禁止该完整 Scope 新增播放，再用有配额的 cancel_owner 游标完成清理。维护、取消和排出按实际槽位（含空槽）计费。接收队列满时保留可交付记录，但继续计费推进游标，使后续取消/失效记录仍可释放。完成一次扫描后不因消费者背压反复自唤醒；消费者释放容量后重新排出。排出以接纳时的 (MediaId, MediaEpoch, Scope) 校验精确业务等待，过滤已失效等待和 Cancelled；输出故障后推进的当前代数不抹去已接纳活动的身份。已结算但尚未交付的 Completed/Stopped/Failed 保留原身份和结果；若延后实际交付，仍需再次校验等待身份。已结算终态不被后来的普通取消重写。输出故障由 fail_output 推进一次后续接纳代数，并通过分片维护将剩余活动按原接纳身份结算失败；宿主负责停止供给。它不代表保位置设备恢复。

output、活动账本和 AudioReclaimService 共用 WakeSignal，宿主先 acknowledge，再排出全部来源，休眠前复核 pending。未扫完的有限工作立即续轮；仍有未结算活动时，poll_after 建议 10 ms 控制侧复查，以补足当前 Playback 缺少逐音轨完成订阅的通知空隙。没有未结算活动时不设置这个周期计时；音频检查不请求布局或呈现。10 ms 是后备调度间隔，不保证过载下完成延迟。

本 PR 的限定 Rust 范围已通过独立验收，覆盖 Linux 离线真实 Kira、默认/关闭 CPAL 路径、终态保留与背压公平释放，以及后台 PCM 释放预算。seek/reload、保位置重建、单活动 pause/finish、分组混音、完整持久描述及脚本会话接线未实现；实体设备、硬件完成时钟与 0.5 ms 控制耗时目标未验收。


## 音频 owner 物理退休凭据（PR #56，限定 Rust 独立验收通过）

AudioActivityRequest 使用服务注册的 AudioOwner 代替裸 Scope；凭据保留完整 Scope 与服务身份，外服务或已关闭 owner 不得准入。retire_owner 先关闭准入，再按持久游标分片取消；业务等待必须先撤销。游标完成仅代表取消扫描完成，不代表内存或描述符释放。

每个已接纳活动分别计资源与描述符义务。全部 PCM 别名释放、最后共享 Residency 的 BudgetTicket 归还之后，才发布资源完成标记；Entry 的 Playback 和资源 key 析构后才发布描述符完成。AudioOwner::is_retired 还要求准入已经关闭。逻辑终态、排空队列、全局共享预算余额均不能替代该精确证据。服务析构关闭 owner，但已取走 fill 或挂起输出仍可能延长资源寿命；PCM 继续经原后台回收路径释放。

owner 注册与活动凭据各有 max_requests 容量上限，保留已完成凭据仍占槽；克隆共享同一登记。相同 Scope 的现存关闭登记不会重开，所有引用消亡后的重新注册是新身份，Session 宿主不能把已退休 Scope 重用为原 owner。现阶段只提供 Rust 媒体侧证据，尚未接入 SessionVm 的外部资源退休门或作者媒体等待。作者限定测试和 32 位编译已通过；独立默认/关闭输出配置各通过 44 项原测试、37 项评审测试，并通过三条实际 64 MiB 释放路径。独立审查未重跑 Clippy/i686；实体设备、会话接线与回收时限未验收。

### ASB 声明映射验证边界

reader 从已验证 B01 的 GlobalDef.linked_function 恢复具名模块函数，核对模块、名称、不可变 global、无捕获及唯一声明映射。具名函数在顶层执行前完成链接，支持合法的函数循环引用。一次程序扫描禁止代码初始化或改写这些声明 global；工程 wrapper 的调用顺序与全部源码初始化器逐项核对。扫描和有序表操作计入工作额度。

声明映射来自实际链接绑定和源码映射，嵌套函数不能仅靠同名元数据成为模块入口。编译器更改声明结构时，同步 reader 规则与对应行为验收。

5138fa9 的 37 项作者静态测试及 16 项独立回归均通过，两个原始入口反例均被拒绝；前缀关联、控制流/异常区、跨函数重复写和预算边界已复验。独立 Clippy 与 reel-package 的 wasm32（32 位指针）编译检查通过，但未执行 32 位代码。Linux x86_64/overlayfs 上已独立观察 12 路不覆盖竞争仅一个完整产物胜出、80 次覆盖的并发读取只见完整旧/新文件；这些结果不代表其他文件系统、平台或断电持久性，也不构成 VM/剧情运行验收。

### 集合展开的执行与成本

Aside 的数组和表展开均由 VM 续体分片执行，源内容版本在捕获期间保持一致。
编译器在每个展开位置完成浅捕获，再执行后面的字段或元素表达式；内部引用共享。
表的显式键和值全部求值后再构造，错误和取消沿既有帧清理释放私有快照及结果。
B01 的 TableSources 保留 Pair/Spread 次序，固定表描述符保持原样；空表复制不逐项查重，非空表合并仅扫描目标原有条目。
数组与空表复制的工作量和载荷分配由 aside-vm 的 collection_copy_cost 检查核对，
源码、B01、舞台与真实对白等待的贯通入口是 [collections.aside](../examples/collections.aside)。
