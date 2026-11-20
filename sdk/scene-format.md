# Moni Scene v2（`.mnsc`）

中文 | [English](scene-format_en.md)

`.mnsc` 是 Moni Engine 的声明式场景源格式，当前使用 JSON。它不是运行时快照，不保存 tick、事件、脚本错误、JavaScript 堆或 GPU 状态。加载器读取 v1 和 v2，保存时始终生成 v2。

## 文件头

```json
{
  "format": "moni-scene",
  "version": 2,
  "name": "example",
  "extensions": {}
}
```

加载器拒绝其他 `format`、未知版本、空场景名、重复或非正实体 ID、空实体名以及未知根字段。扩展名只用于识别，加载时以 `format + version` 为准。

## 根字段

| 字段 | 必需 | 含义 |
|---|---|---|
| `format`、`version`、`name` | 是 | 格式标识、场景 schema 版本和显示名 |
| `viewport` | 否 | 逻辑视口尺寸和背景色 |
| `camera` | 是 | `perspective` 或 `orthographic` 相机及位置、目标、裁切范围 |
| `lights` | 否 | 方向光与点光源数组，默认为空 |
| `extensions` | 否 | 不属于核心场景模型的原始扩展数据 |
| `entities` | 是 | 场景实体数组，每项包含 `id`、`name`、`components` |

相机使用 `projection`、`position`、`target`、`fov_y`、`near`、`far`；正交投影另使用 `orthographic_height`。

## 组件

v2 使用带命名空间的组件 ID：

- `core.transform`：2D 的 `x`、`y`、`rotation`，缺省为零值。
- `core.sprite`：2D 矩形或圆形及尺寸、颜色和图层。
- `core.model`：`cube`、`plane` 或 `fbx`，包含模型变换、颜色、`metallic`、`roughness`。
- `core.script`：`source` 外部 JavaScript 路径或 `inline` 内联源码，必须且只能存在一个。

```json
{
  "core.model": {
    "type": "fbx",
    "source": "testing/assets/garden_diorama.fbx",
    "transform": {
      "position": { "x": -1.6, "y": 0.0, "z": 0.0 },
      "rotation": { "x": 0.0, "y": 0.0, "z": 0.0 },
      "scale": { "x": 1.4, "y": 1.4, "z": 1.4 }
    },
    "color": "#ffffff",
    "metallic": 0.0,
    "roughness": 0.55
  },
  "core.script": {
    "source": "examples/dist/model-orbit.js"
  },
  "vendor.custom": {
    "preservedWithoutExecution": true
  }
}
```

内置组件由 [`ComponentRegistry`](component-schema.md) 解析。未注册组件保留原始 JSON，不获得执行权限。

## 灯光

```json
{
  "type": "directional",
  "direction": { "x": -0.45, "y": -0.8, "z": -0.35 },
  "color": [1.0, 0.96, 0.9],
  "intensity": 1.0,
  "shadows": true
}
```

点光源使用 `position`、`color`、`intensity`、`range`。方向光的方向指向光线传播方向，阴影只由方向光产生。

## v1 → v2 迁移

v1 的 `transform`、`sprite`、`model`、`script` 分别迁移到 `core.transform`、`core.sprite`、`core.model`、`core.script`。其他组件键原样保留，根与实体扩展字段补为空对象。迁移链只处理相邻的 v1 → v2，不包含虚构的后续版本。

运行时快照同样读取 v1 和 v2：升级时增加根与实体 `extensions`，当前 `SNAPSHOT_VERSION` 为 2。`source` 外部脚本路径在场景往返中保留，不会被无意义地改写。

## 虚拟资源路径

根级 `extensions.res` 声明虚拟资源别名表，别名映射到 workspace 相对路径：

```json
{
  "extensions": {
    "res": {
      "terrain": "testing/assets/benchmark.mnec"
    }
  }
}
```

`core.model` 的 `source` 随后可以使用 `res://terrain`。别名即资源的稳定标识：物理文件移动或改用 cooked 格式时只需更新别名表，场景与缓存键保持不变。别名限定 1-64 个字母、数字、`-`、`_`、`.`，不允许 `..`；目标路径必须是 workspace 相对路径。加载器在场景加载时解析别名表并缓存到世界，渲染帧内不做 JSON 解析。`core.script` 的 `source` 仍使用物理相对路径，不走虚拟寻址。

## 往返命令

```bash
cargo run -- scene testing/assets/benchmark-3d.mnsc --output target/roundtrip.mnsc --name example
```

该命令加载、校验并保存场景，可用于验证迁移、别名表和未知数据保留。可运行示例位于已入库的 `testing/assets/benchmark-2d.mnsc` 和 `testing/assets/benchmark-3d.mnsc`。

## 当前边界

v2 不包含预制体继承、动态第三方组件执行、场景增量编辑或二进制编码。加载器位于 `src/scene.rs`，输出经过现有世界校验的 `World`；运行时快照与 `.mnsc` 仍是不同格式。
