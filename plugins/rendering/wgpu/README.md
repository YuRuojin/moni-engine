# wgpu 渲染插件

中文 | [English](README_en.md)

`mod.rs` 与四个 WGSL 文件实现当前 2D/3D/阴影渲染。该目录是渲染提供方的实际代码位置，通过 `core.render` 静态注册到宿主。

## 当前能力

- 2D 精灵和编辑器 overlay。
- 内置网格与 FBX/cooked mesh。
- 正交/透视相机、深度测试、方向光、最多 8 个点光源。
- 基础色、金属度、粗糙度材质。
- 1024 方向光阴影与 3x3 PCF。
- FBX GPU 顶点缓存按 (来源、颜色、资源修订) 跨实体共享，按引用计数在无实体引用时释放；每实体仅保留模型矩阵、绑定组与共享句柄。
- 内置网格与精灵顶点按每实体几何签名缓存，仅在输入或层级顺序变化时重建；编辑器 overlay 顶点仅在 quad 列表变化时重建。
- 每实体模型矩阵、包围球视锥剔除。
- 基准模式 GPU 时间戳。
- 静态场景的一次性 RGBA golden 捕获和像素/元数据一致性检查。
- 编辑器 egui UI 合成：`RenderPluginApi::set_egui_frame` 接收类型擦除的 egui 帧负载 `(Vec<egui::ClippedPrimitive>, egui::TexturesDelta, f32)`（编辑器在产生形状的上下文上完成分块细分后移交；payload 必须与本插件链接的 egui 版本一致，不匹配或渲染器未启动时返回 `false` 且不产生 UI 层）；渲染器持有 egui-wgpu 合成器（表面同格式）与待渲染帧槽位，在主/阴影/overlay pass 之后、呈现之前应用纹理增量并把 UI 直接绘制到同一表面视图，随后立即释放该帧。golden 捕获路径不受影响（golden 模式从不发送帧）。
- 编辑器视口地面网格：y = 0 的 XZ 线格（±20 世界单位、1.0 间距、41×41 条线，X/Z 轴线独立配色），默认关闭，仅经 `RenderPluginApi::set_grid_visible`（与 `set_egui_frame` 相同的 opt-in 工具面模式）切换；使用专用 LineList 无光照管线（共享 `Vertex3d` 顶点布局与 3D 相机绑定组），在主 pass 内 3D 几何之后、精灵/overlay/egui 之前绘制，参与深度测试（实体可遮挡网格）且不写深度，仍在主 pass GPU 时间戳窗口内；golden 捕获从不绘制网格（标志默认关闭，只有该 API 会翻转它）。

依赖：egui-wgpu 0.37 与 egui 0.37（egui-wgpu 0.37 不重导出 egui 本体）。核心库 `moni-core` 保持 egui 无关，UI 帧契约仅经 `&mut dyn Any` 跨越插件边界。

资源解析由 `plugins/assets/` 负责。GPU 资源只在渲染线程创建、使用和释放。

## 限制

没有动态 ABI、纹理、动画、级联阴影、后处理、实例化或跨平台 golden 渲染。GPU 时间戳需要硬件支持，采样只用于基准和编辑器。egui 合成仅支持单视口（编辑器根窗口）。
