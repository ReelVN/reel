# Aside VM 与语言规范
设计版本 1.0 · 已提交独立审查，接口实现状态见正文。

[文档目录](README.md) · [作者指南 中文](guide/zh-CN.md) · [Creator guide English](guide/en.md)

## 一、定位与兼容边界
Aside 是 Rust 实现的动态嵌入式语言，Reel 是首个宿主。语言、编译器和 VM 不依赖窗口、舞台或媒体。工作区使用 aside-vm 与 aside-compiler，源码扩展名为 .aside。本文规定语言和运行协议；节点、剧情、存档槽位及动画库由引擎与 DSL 文档规定。
Quirrel 4.41、提交 19f17df9b6ee43c91ef5c635fd45970ec76c5dbe 是原设计选定的行为参照，开发开始时固定源码、构建选项、测试和许可记录。它用于验证动态对象、闭包、异常、协程等成熟机制，Aside 不宣称源码或字节码兼容。现代语法及本文明确的语义调整优先；对照测试分别标记“保持”“语法转换后保持”“有意改变”，禁止用旧测试通过代替新规则验收。
保留动态类型、共享引用、class、delegate、metamethod、异常、generator、协作 thread、Future、解构、类型注解和模块。语言不引入 Rust 的所有权标注、借用检查、生命周期、trait 或泛型单态化。首版采用解释执行，不预设 JIT、NaN-boxing 或多线程共享脚本堆。

## 二、统一语法
函数统一使用 fn；async 是函数修饰符；剧情标记是宿主元数据，不能另造 story/function/local 等同义声明。普通语句使用分号。换行不插入分号；函数、类和作为语句的控制流块后不用分号。块末无分号的表达式提供块值，省略时为 null。return、break、continue、throw、yield 均以分号结束。
```aside
use reel::{character, narrator, stage};
use game::ui::{dialogue_view};

fn add(a: int, b: int = 1) -> int {
    a + b
}

#[story(id: "chapter01.opening")]
pub fn opening(view) {
    let mio = character("Mio");
    mio.say("先别开灯。");
    narrator.say("雨水沿着玻璃滑落。");
}
```
标识符、字符串和注释采用 UTF-8；注释为 // 与 /* ... */。字符串用双引号，支持转义；插值写 f"音量：{volume}"，花括号字面量写 {{ 与 }}。数组用 [a, b]，动态表用 #{name: "Mio", score: 0}，计算键写 #{[key]: value}。表、参数和导入列表允许末尾逗号。无裸字符串剧情语句；旁白必须调用 narrator.say。宿主可注册属性元数据模式；Reel 的 #[story(id: "chapter01.opening", title: "窗上的光")] 要求 id 为唯一稳定字符串，title 为可选显示标题，未知参数或重复 id 报错。
控制流为 if condition { ... } else { ... }、while condition { ... }、for item in values { ... }，条件不必加括号。for 遍历值；每轮建立新的迭代绑定，循环内创建的闭包捕获本轮 item，不共享最后一轮的槽。键值遍历写 for [key, value] in values.entries()。区间 a..b 不含终点，a..=b 含终点；只接受整数。break 和 continue 指向最近循环。if 可作表达式，作为取值表达式时必须有 else。

## 三、值、绑定与对象
基本值为 null、bool、64 位有符号 int、64 位 float、不可变 UTF-8 string、array、table、class、instance、closure，以及 generator、thread、Future、weakref 和注册宿主值。容器与实例有稳定身份；赋值和参数传递共享其对象，不隐式深复制。
let 禁止重新绑定名称；let mut 允许重新绑定。该限制是浅层的，不冻结对象。函数参数默认不可重绑，可写 mut 参数。字段是否可写由对象类型决定。
```aside
let state = #{score: 0};
state.score = 1;             // 合法：修改同一个对象
let mut selected = "audio";
selected = "reading";        // 合法：重新绑定名称
let increase = |amount| { state.score += amount; };
let mut count = 0;
let next = || { count += 1; count }; // 闭包共享 count 的 cell
```
词法作用域允许内层遮蔽；未声明名称报错。捕获可变绑定时所有闭包与原作用域共享 CellId，离开原函数不复制最终值。不可变绑定可按值捕获；其对象身份仍共享。解构支持 let [a, b] = pair; 与 let #{name, score: points} = value;；表缺省字段写 score: points = 0。缺少必需项或数组长度不符报错；首版不引入 tuple。
表字段访问与字符串键索引等价；= 对表新增或覆盖字段，remove(key) 删除字段。数组越界和不存在的直接字段读取报错；get(key, default: null) 允许明确缺省。实例只允许声明字段。?. 仅处理接收者为 null，?? 仅处理左值为 null；二者均短路。
条件、!、&&、|| 要求 bool，避免隐式真假转换。数值运算允许 int/float 混合，结果按浮点规则；int 溢出和除零报错。+ 接受数值对或字符串对，不隐式字符串化。标量相等按值，容器相等按身份；跨 int/float 比较按精确数值等价，不能先把大整数截成 float。其余算术、比较、位运算和复合赋值有独立算子测试。类型注解采用 x: int 与 -> int，支持 any、容器类别、注册类型和 T | null；不改变动态存储模型，在可确定时静态报错，否则参数进入和返回时检查。

## 四、函数、类与调用
闭包写 |x, y| expression 或 || { statements }。fn 声明绑定不可重绑。位置参数必须在命名参数前；fn f(a, b = 1, ...rest) 支持尾部位置变参。callee、位置实参、命名实参、尾随 children 依源码顺序各求值一次，随后绑定签名。重复名、未知名、多余参数和缺失必需项报错。
默认表达式在每次调用时按参数顺序求值，可引用前面的参数，不可引用后面的参数；因此 children = [] 每次获得新数组。默认表达式不能等待或 yield。调用方实参先全部求值，再执行缺失参数的默认表达式。动态闭包与宿主函数提供同一签名描述，命名调用不能只对内建函数有效。
class 使用单继承，构造方法为 fn init(self, ...)，方法显式声明首参 self；类调用自动创建实例并调用 init。let/let mut 声明只读/可写字段，字段初始化式每个实例按基类到子类顺序执行一次。未写初始化式的只读字段须在 init 中赋值一次，构造结束前检查。只调用最派生的 init；父初始化用 super.init(...) 显式调用。static let 保存只读类字段，static fn 没有 self。方法取值形成保留接收者的绑定闭包，移作回调仍调用同一对象。只有类方法使用接收者调用约定，表中的普通闭包不自动注入 self；普通函数不继承调用者的隐式 this。
```aside
class Counter {
    let mut value = 0;
    fn init(self, start = 0) { self.value = start; }
    fn increment(self, amount = 1) { self.value += amount; }
}
let counter = Counter();
let click = || { counter.increment(); };
```
继承写 class Child: Parent，父方法用 super.method(...)。delegate 是表的显式查找后备链，set_delegate()/get_delegate() 管理，拒绝形成环；自有槽优先。metamethod 保留统一的 _get、_set、_call、_add、_cmp 等命名契约，缺省操作与递归防护逐项测试。构造、比较、属性访问和其他 metamethod 均是不可挂起边界；不能在运算中偷偷等待外部输入。这些 receiver、字段初始化与赋值规则属于相对 Quirrel 的有意调整。

## 五、模块与诊断
use reel::{stage, text}; 是分组导入，use game::ui::dialogue::dialogue_view; 是单项导入，use reel::prelude::*; 导入该模块全部公开名字，三者使用同一名字解析规则。game 为项目根命名空间，其他根名来自项目清单的依赖别名；路径只允许标识符段。别名写 use game::ui::{preferences as settings_view};，跨模块导出使用 pub；通配导入不得静默覆盖已有名字，重名必须显式选择或起别名。项目清单指定的启动函数是显式宿主入口，不要求 pub；这不使其他私有符号可被导入。不同时保留 from/import、require 和字符串路径三套作者语法。
项目源码根目录是 game.toml 所在目录。game:: 后的模块段对应相对目录与 .aside 文件，文件名和目录名必须是合法标识符。单项导入的最后一段始终是导出符号，此前各段组成模块路径：game::ui::dialogue::dialogue_view 从 ui/dialogue.aside 导入 dialogue_view；分组导入的花括号与通配导入的 * 之前均为模块路径。外部源码根通过 game.toml 的 [modules] 表配置别名到相对目录的映射，game 与 reel 为保留别名；reel 由宿主注册，不作为源码目录查找。配置入口 entry = "main::main" 从项目根的 main.aside 选择 main 函数。
模块实例包含固定版本的导入绑定与私有全局。源码先编译并解析依赖，再按依赖顺序执行顶层初始化；首版拒绝初始化依赖环并显示完整环路。初始化不得等待外部事件。新模块版本创建新实例，旧闭包继续持有旧代码与旧模块实例；重载不改写正在运行的帧。
诊断使用稳定代码、严重性、模块版本、主源码范围、相关范围、调用栈及可选修复建议。文件坐标保留 UTF-8 字节范围与展示行列；中文字符不能造成标注错位。动态调用错误标出实参及签名，等待失败标出 await 和原始失败位置。提供人读输出与结构化输出，不把 Rust panic 或内部地址直接展示给作者。错误分类覆盖语法、名称、类型、参数、只读写入、不可暂停边界、无效句柄、资源配额及恢复适配缺失。

## 六、children 与属性绑定
尾随 children 块是通用调用语法，等价于为名为 children 的参数构造数组，不是隐式闭包，也不按帧重跑。
```aside
fn panel(title, children = []) {
    column(gap: 12) {
        text(title);
        for child in children { child; }
    }
}
let view = panel("声音") {
    text("音乐");
    slider(value: bind(settings.music), min: 0, max: 1);
    text(value: derive(|| f"音量：{settings.music}"));
};
```
块内独立表达式语句返回的 Node 按顺序收集，分号不取消收集；块末无分号的 Node 表达式也仅收集一次。null 忽略，其他值报错。let 初始化和赋值不收集；if、for、while 及普通语句块继承词法收集位置，但控制流整体不再重复收集其末值。嵌套尾随块建立自己的收集器；被调用函数不继承调用者的收集器。显式 children 实参与尾随块不能共存。
块立即执行一次，得到完整 children 后调用组件；构造过程不得等待外部事件，预算让出仍可保存临时构造帧。构造函数返回未挂载 Node，挂载和单父规则由宿主验证。闭包参数必须显式命名传入，不能把任意尾随块猜作事件回调。
bind(model.volume) 是编译器识别的可观察属性位置表达式，生成 PropertyRef；其能力为 Read 或 ReadWrite，由宿主属性定义决定。text 等展示接口接受可读引用，slider 等编辑接口必须要求 ReadWrite；只读引用不能隐式升级。宿主必须为对象注册稳定属性身份、读取、可选写入和版本接口。Reel 的 model(#{...}) 创建默认可写的可观察业务模型；对话文本和控件交互状态可注册为宿主管理的只读属性。普通表字段、临时值与 a + b 不是可观察属性，不能使用 bind。对象和键只在创建引用时求值一次，之后引用同一属性位置；若需随着对象变化重新选择位置，应重建绑定。
derive(|| expression) 创建只读 DerivedBinding。求值期间记录实际读取的可观察属性，在依赖变化时按版本失效并重算；条件变化后替换依赖集合。禁止写入可观察状态、修改外部对象、启动活动或等待；调用宿主函数还要求其注册为派生安全，违规给出派生源位置。普通数组和局部变量不是自动响应状态；需要持续更新的数据放进宿主属性模型。只读派生不能传给 slider 等要求写入能力的接口。派生依赖环报错，销毁或替换绑定时解除订阅。跨属性通知、约束与提交顺序由宿主实现，VM 不包含 UI 调度器。

## 七、暂停、Future 与协程
所有普通 fn 在当前 Task 的可恢复调用栈执行，允许 await；是否有任务上下文由调用入口决定。Reel 的入口与 UI 回调均由任务执行。调用 say 等已注册的可暂停宿主函数可以挂起整条普通函数链；这是一项明确的宿主能力，不要求每层 helper 改写为 async。
async fn 调用立即返回 Future，函数体在后续调度片开始。它创建带 ParentTaskId 的新任务，继承当前显式 OwnerId；普通调用栈返回不取消该任务，所有者结束或显式取消才终止它。丢弃 Future 不等于取消。需要不同生存范围时使用显式 spawn(owner: ..., task: || { ... })。宿主必须提供 owner；无主后台任务不被悄悄创建。
await 接受统一 Awaitable 协议，Future 与 activity.finished 都适用；不接受普通标量。即使已经完成也在下个可调度步骤继续，避免立即完成与稍后完成产生两套重入规则。完成一次后向所有等待者按登记顺序交付。Fault 在 await 位置抛出；活动 Stopped 返回终态结果，Cancelled 抛出取消错误。所有者撤销任务时停止脚本推进，由宿主释放其订阅与活动。
Future.resolve(Future) 继承结果并检测自引用链；async 返回 Future 时采用相同规则。任务 Future 只能由任务返回、失败或取消结算，普通手工 Future 才允许显式 resolve/reject。未被观察的失败产生诊断。线程调度均在 VM 所在线程，不等同于 Rust Future 或操作系统线程。
generator 使用 gen fn，yield value; 只挂起生成器自身，next() 返回 #{done: bool, value: any}；for 可消费生成器。thread 使用标准库 Thread(fn)，call()/resume(value) 驱动其独立脚本栈，suspend(value) 与调用者交换值。二者保留各自状态和异常边界，不混成 async；首版禁止在 generator 或手动 thread 内 await 外部事件，避免出现两个恢复所有者。其对象和挂起帧仍进入快照。
try { ... } catch error { ... } finally { ... } 中，catch 与 finally 至少有一个。catch 捕获 throw 和可处理运行错误；正常结束、return、break/continue 离开 try 或异常展开时，均先执行 finally。finally 不得 await、yield、return 或跳出其自身边界；可按预算让出，但动态进入外部等待仍报错。finally 再次抛出异常时替代原结果，并保留原异常作为 cause。活动取消且等待任务仍存活时，await 抛出取消异常，照常经过 catch/finally；任务自己的 owner 撤销时直接终止任务，不运行脚本 finally，宿主按所有权释放资源。异常值、处理中途结果与 finally 展开状态都进入可恢复帧。原生崩溃不转成可恢复脚本异常。不可暂停边界包含同步宿主重入、默认参数、模块初始化、构造、metamethod、派生求值、finally 和 children 构造；能够静态判断时提前报错，动态路径进入等待时仍必须检查。

## 八、VM 与宿主协议
VM 采用寄存器执行帧、共享函数代码与常量、驻留标识符、可复用参数区。Value 首版使用显式类型标签和数值或代际句柄载荷，64 位目标以 16 字节为布局目标，实际大小由测试确认。对象句柄带代数，不泄露原生指针；跨线程只移交拥有的数据、稳定 ID 或只读缓冲。
run_slice 接收指令额度、分配额度、时钟检查间隔，返回 Returned、BudgetYield、HostCall、Waiting 或 Fault。BudgetYield 是调度让出，不改变脚本结果。HostCall 包含稳定符号、签名版本、TaskId、OwnerId、源位置和借用参数视图；宿主返回 Ready、Pending 或 ScriptError。Pending 必须退出原生调用，再由 VM 保存操作描述、等待身份、返回槽与后继 PC，不能把 Rust 栈当作续体。同步宿主重入建立不可跨越的等待边界。
任务、等待和完成消息带 SessionEpoch、TaskId 与 WaitId；宿主匹配身份后至多交付一次结果，过期结果丢弃。原生可挂起算法必须使用显式分块状态机或后台任务。算法重试只能恢复后继指令，不能重新执行已经生效的调用。宿主注册还声明参数、可暂停性、能力需求以及快照适配版本。
强根包含寄存器、对象字段、cell、模块全局、任务结果与宿主保留根。引用计数处理常见寿命，循环对象由有预算的可达性扫描回收；写屏障覆盖新增强边、根变更和弱引用升级。零计数对象先失效，边拆除和释放进入分片队列。维护队列及待释放字节计入堆预算；接近上限先维护并限制分配，不能只声称“增量”而允许无限积压。回收阶段不执行用户回调。

## 九、编译产物与构建配置
编译器输出版本化模块包，包含语言版本、包格式版本、宿主 ABI、模块导入导出、函数签名、指令、常量、源码映射及宿主元数据。数字宽度、端序和编码固定，加载布局可与磁盘编码不同。反序列化、索引/寄存器/跳转验证、声明额度检查和宿主符号验证全部通过后才能执行；错误包不得触发越界、无限解码或巨额预分配。
player 的 Cargo feature source 引入可选编译器；关闭后只加载字节码，依赖树不得保留编译器、文件监视器或 Quirrel 对照运行时。hot-reload 只控制模块替换协议，与 source 独立；精简播放器也能接收外部编译产物。Cargo cfg 决定二进制能力，运行参数选择已包含能力的工作方式，不以运行参数伪装裁剪。
编译缓存按源码、依赖、语言版本和选项失效。生成代码与运行时闭包实例分离，动态成员缓存绑定对象布局版本，动态命名调用缓存绑定函数版本；旧版本在最后一个帧、闭包或快照引用离开后释放。

## 十、快照与恢复接口
VM 只负责可恢复执行状态和对象图；保存请求、屏障、媒体与磁盘原子提交由宿主协调。snapshot_begin 在安全点固定根和逻辑状态，snapshot_step 按对象、边与字节预算复制到独立快照区；期间业务写入暂停，后台结果排队。字符串和大容器也必须能分块复制。
快照记录模块版本、全部任务和协程、PC、返回槽、异常处理栈、接收者、寄存器与中间值、共享 cell、模块环境、对象共享/环/弱边、Future 结算与等待队列、待交付结果以及原生续体描述。代码、资源描述等真正不可变内容可共享；活动属性与 cell 必须独立记录。绑定保存属性身份或派生闭包，不保存临时通知队列。
宿主值必须注册版本化恢复描述；缺少适配时返回到达该对象的引用路径，不静默丢失字段或把值变成 null。设备对象以描述重建，原生函数按注册符号重连。restore 先在隔离堆创建身份，再连接引用和等待关系，完整验证后才公开。缺少代码版本或 ABI 不匹配要求明确迁移；禁止用新代码 PC 猜旧恢复点。存档捕获、恢复提交与热重载提交互斥。

## 十一、性能与验收
基准覆盖属性访问、闭包共享 cell、命名调用、字符串、对象创建、宿主调用、长分支、连续 await、深链释放和循环回收，记录时间、分配、峰值内存及最慢不可分割操作。指令预算之外，还计容器元素、复制字节、回收边和分配字节，禁止一次原生容器操作绕过调度预算。每 256 个工作单位内检查一次让出条件是起始配置，真实尾延迟实测决定调整。
优先做好编译期寄存器/跳转定位、已知调用绑定、缓存和帧复用；不给每次普通调用附带 UI、存档或全局动态扫描。优化前后跑同一行为集。首版不引入增量存档、移动 GC、整套 Rust 类型系统或并发脚本对象图。
完成条件包括：语法正反例、模块与类型诊断、默认值隔离、对象别名、闭包可变捕获、collector 不重复收集、读写绑定区分、动态不可暂停边界、Future FIFO 与失败传播、协程快照、过期完成消息、多版本代码恢复、异常退出、恶意字节码验证，以及无 source 配置的独立包执行。Quirrel 对照不是兼容口号：每个保持项有来源与测试，每个变化项有新语义与回归用例。

## 参考与变更说明
本版相对原设计主动移除旧源码语法与两套 await 规则，并明确默认参数、实例字段、receiver、数值检查和所有者语义。原 Quirrel 文档描述用于比较，不覆盖本文决定；固定提交行为须在开发首步核验。
Quirrel 项目与测试：https://github.com/GaijinEntertainment/quirrel
函数与闭包参照：https://quirrel.io/doc/reference/language/functions.html
类与实例参照：https://quirrel.io/doc/reference/language/classes.html
async 调度参照：https://quirrel.io/doc/language/async.html
协作 thread 参照：https://quirrel.io/doc/types/thread/index.html
