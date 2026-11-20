# FBX 导入插件

中文 | [English](README_en.md)

`mod.rs` 使用 `ufbx` 读取外部 FBX，输出规范化几何，不执行动画或加载贴图。

## 当前能力

- 二进制/ASCII 由 `ufbx` 识别。
- 节点与几何变换、坐标系和单位转换。
- 面三角化、法线和缺失法线回退。
- 材质基础色。
- 以模型中心和最大边归一化。
- 非有限顶点、空模型和缺失文件明确失败。

该实现只产生几何数据。GPU 上传属于 `plugins/rendering/wgpu/`，异步调度属于 `plugins/assets/runtime/`，cooked 编码属于 `plugins/assets/cooked/`。

## 限制

不支持贴图、骨骼、动画、蒙皮、材质纹理或外部资源打包。导入器当前与引擎静态链接，没有独立 ABI。
