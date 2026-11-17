# 0.3.11 引擎性能计划

桌面目标是让可见演出、滚动和连续推进的活跃帧稳定落在 16.67 ms 内，并缩短切页到首次呈现的时间。资源身份和绘制依赖由共用引擎处理。桌面设备适配负责纹理存储、上传和完成通知。

参考 Knife 主线 `af45593` 的请求复用、菜单底图及纹理回收。 [Knife 源码](https://github.com/AuroraWright/knife/tree/af4559300e35cb2502f87cc9e918edec7d2d151b) · [演出与资源加载](https://github.com/AuroraWright/knife/blob/af4559300e35cb2502f87cc9e918edec7d2d151b/engine-writeup.md#frame-loop-and-timing)

## 本轮改动

- **接管在途预载。** 图片变为可见时沿用原请求，共享图集页一并改用可见优先级。预取退出后可按可见预算重新请求。 [图片请求](../crates/reel-player/src/images.rs)
- **复用独立效果。** 桌面在相邻子树转场时继续复用静态效果。实际参与采样的命令、遮罩、参数、纹理和栅格决定能否提前命中。 [效果来源证明](../crates/reel-render/src/common/effect_result.rs)
- **复用效果管线的准备数据。** 同一 WGSL 代码和绑定布局在顶点输入或混合状态变化时只创建一份模块和布局。 [桌面效果](../crates/reel-render/src/gpu/effect.rs)
- **缩小桌面效果的着色器。** 两个分支都需要的值只生成一次。同形的标量采样累加使用有序循环，采样坐标和权重保留原始浮点值。 [分支](../crates/reel-render/src/common/effect/control_flow.rs) · [采样循环](../crates/reel-render/src/common/effect/scalar_reduction.rs)
- **缩小不透明图的上传和驻留。** 桌面直接上传完全不透明的 sRGB 图片，带透明度的图片使用线性预乘 RGBA16F。1920×1080 图的纹理载荷从 15.82 MiB 降至 7.91 MiB。 [图片成本规划](../crates/reel-render/src/gpu/image_plan.rs)
- **按缺口回收纹理。** 桌面先回收可释放的退休图片，再回收最久未用的空闲纹理。腾够本次分配即停止，候选帧和在途提交保留租约。 [纹理缓存](../crates/reel-render/src/gpu/image_cache.rs)
- **复用虚拟列表行的资源索引。** 已挂载行复用自己的索引，视口覆盖仍根据实际布局和绘制外延判断。 [虚拟列表图片](../crates/reel-player/src/stage_widget/virtual_images.rs)
- **复用 RPK 续读的索引位置。** 纯归档挂载每个读取请求只解析一次资源路径，续片用已确定的归档和条目序号。每片仍为 64 KiB，调度和取消在片间生效。 [归档读取](../crates/reel-player/src/assets/io.rs)
- **裁剪被不透明裁切图盖住的绘制。** 正常混合、全不透明且位于图片内的裁切区域，可用完整覆盖的矩形跳过下层绘制和资源需求。 [不透明遮挡](../crates/reel-render/src/common/occlusion_plan.rs)
- **按需分配音频片段队列。** 排队时才扩充队列并计入控制线程描述符预算。 [音频播放](../crates/reel-player/src/media.rs)
- **复用桌面音频设备探测。** 首次配置沿用创建时取得的设备与支持格式，避免读档创建候选会话时重复枚举。后续显式配置重新查询默认设备。 [桌面音频](../crates/reel-media/src/desktop.rs)

## 继续评估的候选

| 项目 | 实施条件 |
| --- | --- |
| 低分辨率效果链与离屏精度 | 当前按作者倍率和每级输出契约执行。共享低分辨率链需先证明相邻效果的采样域、边缘、量化和逻辑尺寸等价，再对照实际写入像素及 GPU 时间。离屏精度先同格式对照 1× 与 4×。 [效果缩放](../crates/reel-render/src/gpu/replay/effect_resolution.rs) · [离屏成本](../crates/reel-render/src/gpu/targets.rs) |
| 焦点辉光与固定调色 | 缓存键已有源像素和效果参数。仅当重算在场景中持续占用预算时，把焦点变化移到合成端，或缓存实测反复命中的调色变体。 [Knife 辉光](https://github.com/AuroraWright/knife/blob/af4559300e35cb2502f87cc9e918edec7d2d151b/knife/src/game/choice.rs#L703-L753) · [转场缓存键](../crates/reel-render/src/common/retained.rs) |
| 分批预上传、剧情前瞻和冷页面预热 | 先按首次呈现耗时定位 CPU 解码、字形准备和 GPU 上传的占比。后面的资源沿已选剧情准备，跳转时撤回。列表优先用小图，详情再请求大图。 [预载队列](../crates/reel-player/src/images.rs) · [Knife 预览](https://github.com/AuroraWright/knife/blob/af4559300e35cb2502f87cc9e918edec7d2d151b/knife/src/game.rs#L6897-L6966) |
| 自定义效果冷编译 | 继续按效果测量管线创建。检查剩余编译工作能否在相同设备和渲染状态下复用。 [效果管线](../crates/reel-render/src/gpu/effect.rs) |
| 快进转场 | 保持现有转场时长及受保护演出语义。容许跳过的转场需明确授权，执行终态、保留回调，并验证停在未读台词和选项时的行为。 [转场入口](../crates/reel-engine/src/aside/transitions.rs) |

## 验证

用正式桌面播放器运行 DDLC 的 `showcase-story`、`showcase-story-fast` 和 `showcase-fast`。完整场景覆盖正文转场、历史滚动、快进、存读档、Lucy 历史、Euphoria 设置、诗歌、桌面应用、视频和小丑牌开场至牌桌。观察活跃帧的提交间隔和宿主工作，静止页面按输入到呈现的时间计。报告分别标记提交、显示和 GPU 完成，像素对照使用同一命令序列。档位、场景和运行报告存放于 `target/reel-artifacts/`。 [性能工具](../crates/reel-dev/README.md) · [离屏场景测试](../crates/reel-cli/tests/project_scenarios.rs)
