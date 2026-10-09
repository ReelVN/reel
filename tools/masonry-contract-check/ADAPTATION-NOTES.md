# 固定上游的适配边界与建议

本文是源码分析及接口建议，不是生产接口定义。来源均为 Xilem/Masonry `271a27a6d4a930f7878d404f9014e3c50a3a9b88` 和 lockfile 中 Imaging 0.0.1；作者指南由负责人维护。输入补丁仍限于实验目录，本文没有实现新渲染器或作者滑条。

## 独立层：优先完整记录流，缓存模式由 Masonry 修正

**当前 Scene 路径建议由 Reel 适配器保留完整记录流，并由后端实现组的离屏合成。** `IsolatedScene` 是画序分段，不能把每个片段当成独立、平衡的绘制单元。现有 `VisualLayerPlan::replay_into` 已是公开扩展点；图片、普通控件和作者组可统一进入同一个拥有型记录 sink。没有必要仅为这条路径新增 Masonry 绘制 fork。

已有实验实际验证：两个透明度 0.5 的祖先组、重叠图片和真实按钮的独立层片段串联后 alpha=64；单独校验其中两个片段失败。新增日志直接记录各片段的命令及跨片段组/裁剪深度，见最终 verification.log。External 的下列结论来自源码，尚无 External 设备或运行用例。

精确来源与责任：

| 固定上游位置 | 记录行为 | 最小责任边界 |
| --- | --- | --- |
| `masonry_core/src/passes/paint.rs:41–65` | `finish_current_layer` 移走当前 Scene；没有关闭或重新打开活跃组/clip | Reel 按片段顺序重放进同一 sink，跨片段保留作用域；不对每个片段单独渲染 |
| `passes/paint.rs:98–148`；`core/contexts.rs` 的 `PaintCtx::set_paint_layer_mode` | 每轮遍历重置 Inline，缓存回调没执行时没有机会重声明层模式 | 若使用稳定 IsolatedScene/External 边界，由 Masonry fork 修正 retained 模式；完整 Scene 串联本身不要求稳定片段数 |
| `passes/paint.rs:151–166,272–281` | 在子树进入/离开时切片；层 transform 仍是所属 layer root 的变换，不是 isolated 控件自己的变换 | 进入/离开是分段标记，不是 Group Begin/End，也不隐含新的裁剪空间 |
| `passes/paint.rs:169–196,243–256` | 顺序严格为 pre → PushClip → paint → children → PopClip → post | 保留此顺序；几何 clip 覆盖自身 paint 和子树，自己的 pre/post 在该 clip 外，但仍受祖先 clip 影响 |
| `passes/paint.rs:168–171,197–211,267–269` | External 跳过自身 pre/paint/post 记录的输出及自身 clip，仍递归 children，随后插入 placeholder | placeholder 不代表一个保留全部本地绘制语义的外部子树；不能事后恢复已经省略的任意组/clip |
| `app/visual_layers.rs:25–33,78–90` | replay 跳过 External；placeholder 只有 WidgetId、层 transform 和 bounds | Reel 若消费原生 External，必须显式处理该项；只调用 replay 会漏画 |

最小缓存模式修正建议：把模式作为 retained 声明（初始化 Inline），缓存回调未运行时沿用；控件显式切回 Inline 时才清除。相应修改 `paint.rs` 的无条件重置和 `contexts.rs` 的“每次绘制必须重声明”文档。若坚持回调默认模式，则需逐 pre/paint/post 缓存模式并定义合并优先级，范围更大。**不能仅把重置移到“任一回调执行”的 if 内**：仅 pre 重绘而设置模式的 paint 没重跑时仍会丢模式。本轮只给方案，未叠加绘制补丁。

完整流适配的建议边界：

1. 以一个 RenderFrame 为命令及资源拥有边界。逐层调用 `record::replay_transformed` / `plan.replay_into`，保留 Draw、Push/PopGroup、Push/PopClip 和诊断 Context 的原顺序。GroupId/ClipId 是各 Scene 的局部表索引；重放到同一 sink 重新登记 payload，不能直接跨 Scene 复制这些 ID。
2. Group Begin 保留 composite、clip、mask、filters；Group End 才将整组结果合成一次。片段换层时只切记录来源，不结束效果组。把祖先组复制到每个独立片段并各自应用 opacity，会使重叠 alpha 改变，不能作为修复。
3. clip 保留 shape、fill/stroke、变换和作用域；不能只取全局包围矩形。Masonry 存的是 border-box clip；转换为窗口空间用 `layer.transform * border_box_to_layer_transform`。作者传入 content-box clip 的 inset 平移已经在 `LayoutCtx::set_clip_path` 完成，不能再加一次。
4. 非隔离 PushClip 与 Group 自带的结果 clip/mask 含义分别保留。后端可做成组树或保持开着的离屏目标；暂停/继续片段必须共用同一组结果，而不是反复合成。最终帧才检查完整作用域闭合，不能把合法的 pre/post 缓存片段当完整 Scene 验收。
5. 记录转换与 PresentedGeometryView 来自同一布局/记录候选；稳定 NodeId/WidgetId、资源版本和预算票据由 Reel 衔接。不要用下一次布局重新解释旧帧的 transform/clip。

若确实要原生 External / 独立层作为生产协议，现有 plan 信息不足。最小 Masonry 侧建议是在 `paint.rs` 的真实作用域位置记录 External 边界，`visual_layers.rs` 明确暴露：**实际内容到层空间变换、clip 链/组作用域关联、画序位置及拥有节点**；自身 pre/post 及自身 clip 不能在记录前被丢弃。资源实现和效果合成仍归 Reel。它可保留完整 Begin/End 流，也可暴露语义等价的作用域树；不要只给独立 Scene 添加平衡用的前后缀。

如果外部内容完全由 Reel StageGraph 的作者节点及效果声明产生，Reel 可以自己记录这些节点及作用域，不使用缺信息的 Masonry External placeholder；这需要负责人选定唯一画序来源，不能从现有 placeholder 猜出已经省略的作者/控件绘制。无论哪种方案，不能把 `state.border_box()` 直接当成窗口坐标：placeholder 当前写入的是控件局部 border-box，而计划声明 bounds 为层局部；控件的 origin/transform 还需明确换算。以上原生 External 修正尚未实现。

## Slider：行为可以复用；完整几何与状态投影需要小接口

**水平 slider 的作者 track/thumb 可由公开 Widget 包装器承载，复用真实 Slider 交互，禁用默认绘制；完整方向/轨道端点/值投影协议宜在 `masonry/src/widgets/slider.rs` 增加最小接口。** 不建议重新写拖动、键盘和 AccessKit 行为。本文只做源码判断；没有把这个包装方案标为已运行的作者滑条。

| 项目 | 已有公开扩展点 | 约束 / 建议 |
| --- | --- | --- |
| 指针、键盘、访问动作 | `Widget::on_pointer_event/on_text_event/on_access_event`，对应 Slider `:144,:179,:247` | 包装器以同一 EventCtx、同一 Action=SliderMoved 委托；按键步进、Shift、Home/End、Increment/Decrement/SetValue 继续用上游实现 |
| 默认绘制 | Slider `pre_paint:337`、`paint:368`，Widget 的默认 post_paint | 包装器独立实现全部绘制阶段，不能只跳过 paint：pre 还会画背景、阴影、focus/hover 边框，默认 post 也可能画边框 |
| track/thumb 节点 | 公开 register_children/layout/children_ids 与 WidgetPod | 包装器拥有作者节点，并正常注册、布局与绘制；Slider 自身 children_ids 为空，不能直接塞进它的私有字段 |
| 几何 | EventCtx 的尺寸/逆变换、Props 的 ThumbRadius/TrackThickness 可查 | 没有公开 `thumb_rect/track_rect/progress` helper；指针只按 local x/content width，默认绘制却内缩半径，见下文 |
| 属性 TypeId | PropertiesRef 按属性类型查找；property_changed 可转发 | Wrapper 的 Widget TypeId 不等于 Slider。默认值和 class stack 按实际 Widget TypeId 选，不能靠委托假装继承；对 wrapper 显式登记 defaults/stack 或使用显式 props，并声明 UsesProperty |
| focus/hover/active | accepts_focus、Ctx 状态查询、Update::FocusChanged/HoveredChanged/ActiveChanged、class 状态 | 控件 WidgetState 和捕获仍只有一个 owner；作者外观读取状态投影。装饰槽位须不抢命中/焦点，或由 wrapper 明确阻止向装饰子节点传播指针；否则 hover 可能落在 thumb 而不是 slider |
| accessibility | Slider `accessibility_role:442`、`accessibility:446`，AccessKit Node 可修改 | 委托保留 Role::Slider、值/范围/步进及动作；wrapper 可补名称、覆盖方向、处理装饰 children。节点初始 children 在 `passes/accessibility.rs:112` 建立，widget accessibility 随后可调整；可访问动作必须转发，不能只转发树构建 |
| 静默更新与只读值 | 公开构造器和 `Slider::set_value/set_step/set_range` | setter 要求 WidgetMut<Slider>；没有 Wrapper→内嵌 Slider 的公开 WidgetMut 投影，也没有 value/min/max/step getter。生产双向绑定不应靠私有字段或另一套 authoritative value 绕过 |

真实 Slider 当前的几何不是一个可直接复用的“默认滑块命中形状”：

```text
Pointer (slider.rs:71): p = clamp(local_x / content_width, 0, 1)
Default paint (:408–424): thumb_center_x = r + p * max(content_width - 2*r, 0)
```

例如 W=100、r=10、p=0.25，默认中心在 x=30，但按下该中心输入求得 0.30；不能宣称默认中心与输入映射已经一致。控件命中仍是矩形，没有专门圆形 thumb hit。若保留现有输入，作者中心用 `p*W` 才是同一映射，端点半个 thumb 会超出轨道；布局/clip 必须明确允许这一点。若作者希望内缩轨道，则应共享端点 a/b，由同一个几何 helper 同时算中心 `a+p*(b-a)` 和输入逆映射；不是另造拖动逻辑。宽度、有限值、范围退化及 step 规范化也应由负责人定一个规则。

方向同样有边界：上游 pointer 固定 x、measure 按水平、accessibility 固定 Horizontal；键盘 Right/Up 加、Left/Down 减。旋转整个组会自然改变物理输入到 local x 的映射，但这不是公开的 vertical/reversed 配置；仅覆盖访问树 orientation 也不会改变输入算法。若需要真正的 axis/direction，复用现有事件代码，将轴投影和键盘方向放入共享 helper，访问树从同一配置给出方向。

最小 widgets 修改建议仅从 `masonry/src/widgets/slider.rs` 开始（没有实现）：

- 公开只读当前规范化值/范围/step，以及不发 Action 的内嵌投影入口；现有 WidgetMut setter 调用同一规范化 helper。这样 StageGraph 继续是业务值唯一权威，SliderMoved 写入有序事务，控件仅保存交互投影。
- 若需端点/axis/direction，公开纯几何描述或 helper；现有 pointer 与默认 paint 共用。作者槽位仍由 Reel wrapper 布局，不必把 StageGraph 作者节点 API 塞进 Masonry。
- `set_range:115` 目前仅在数值实际变化时经 set_value 请求 render；范围改变、数值不变仍会影响外观/访问范围。生产投影入口必须明确触发布局/绘制/可访问性更新，不能照搬该失效条件。

已有公开构造器可以重建内嵌 Slider 作为受限的静默值投影尝试，但没有 getter 提供其规范化结果，step 还按 `round(value/step)*step`，并非以 min 为起点。此路径没有运行验证，不作为推荐生产接口。保留真正 Slider 为独立子 WidgetPod 的方案能直接用 setter 和原 TypeId，却不能从外部关闭其固有 pre/paint；透明颜色不足以消除轨道/边框，也不是稳健的作者皮肤方案。

如决定修改 widgets，生产快照闭包需另审 masonry crate 所需模块、文档 include 资源和同级宏依赖；不能认为只复制 slider.rs 就能构建。本轮未导入 widgets 修改、未复制完整生态，也未改作者指南。
