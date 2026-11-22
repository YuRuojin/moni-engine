# 场景编辑器插件

中文 | [English](README_en.md)

`mod.rs`、`document.rs`、`project.rs`、`scripts.rs`、`ui.rs`、`ui_egui.rs` 和 `viewport.rs` 实现 `editor` 命令的场景编辑器。启动时组合根解析项目上下文（见下文"项目模式"），并把合并后的 profile 交给编辑器会话；组合根在会话构造时安装 QuickJS 运行时——非播放状态脚本组件保持为数据，进入播放模式后脚本随模拟 tick 执行。

## 项目模式

- 项目检测：`moni editor` 在含 `game.mngp` 的目录运行时自动采用该工程；`--project PATH` 打开其他目录的工程，并把进程工作目录**全局**切换到工程根——此后所有 workspace 相对路径（文档、资产、脚本、存档）都解析到工程根内。在工程目录内运行不需要 chdir。编辑器不提供 `--profile` 覆盖，工程声明的 profile 始终是基础。
- profile 合并：工程的 patches/config 先应用，随后编辑器强制启用自己的工具插件（`core.render`、`core.editor`、`core.simulation`、`core.fbx`，覆盖工程对工具插件的停用补丁），最后追加渲染配置补丁（`collectGpuTimestamps` 与 `hotReload`；键级合并，后写覆盖）。检测与合并逻辑在组合根 `src/main.rs` 的 `resolve_editor_launch`，含单元测试。
- 默认场景：工程的主场景（`game.mngp` 的 `scene` 字段）；`--scene` 仍然覆盖默认值。无工程时保持 workspace 模式（`editor` profile + 基准场景）。
- 项目面板（`ui_egui.rs`）：显示当前工程名称/profile/根路径（无工程时显示 "no project — workspace mode"）；"Open project..."（路径输入框）与 "New project..."（经 `project.rs::spawn_new_project` 以子进程运行 `moni new <path>`，复用 cook 导入器的超时/状态模式）打开工程；最近工程列表持久在用户配置目录 `%APPDATA%`（Windows）/ `$HOME`（unix）→ `.moni/editor-recent.json`（上限 10 条，路径去重移到队首）。打开流程：验证 `game.mngp` 与主场景文件 → chdir → 经文档的 `open_scene` 守卫路径重建文档状态（历史、选择与脏锚点复位）；只有完全成功的打开才记入最近列表。
- 工程打开成功后自动触发一次 TypeScript 构建（见"Scripts 面板"）；自动构建只在交互会话进行，`--frames` 自动化运行不触发。

## 面板

- 播放工具栏：Play / Pause / Resume / Stop、PLAYING/PAUSED 状态指示与当前 tick；空格键在播放/停止间切换。
- 层级面板：实体列表（id + 名称）选择，创建、逐实体删除与重命名。
- Inspector：名称、Transform（拖拽值按 change-commit 应用）、附加动态组件按注册 schema 驱动字段（数字范围、枚举下拉、颜色/路径/字符串文本、对象 JSON），组件添加（值按 schema 默认预填）与移除；替换组件即重新附加新值。
- 撤销/重做（Ctrl+Z / Ctrl+Y 及带深度的按钮）、脏标记、保存（Ctrl+S）与另存为。
- 资产浏览器：扁平分类列出工作区的场景（`.mnsc`）、模型（`.fbx`/`.mnec`）和脚本（`.js`/`.ts`），最多下探 3 层目录、上限 500 条，跳过 `target/`、`node_modules/`、`.git/` 与 `examples/dist/`；列表按会话 revision 缓存（播放期间不重扫），Refresh 按钮强制重扫。场景条目 Open、模型条目 Spawn（生成带 FBX 组件的实体，`.mnec` cooked 源与 `.fbx` 一样通过 `Model3d` 校验）、脚本条目 Attach to selected（读取文件内容附加到选中实体，读取前做大小上限检查，引擎的 `script_too_large` 仍是权威校验）。
- Scripts 面板（`scripts.rs`）：Build 按钮在工程/workspace 根运行 `npm run build -- --pretty false`（Windows 用 `npm.cmd`），stdout+stderr 在工作线程捕获，构建期间 UI 不阻塞；上限 120 秒超时杀掉挂起的构建。输出解析为结构化诊断列表 `{file, line, col, code, message}`（宽松匹配 `path(line,col): error TSxxxx: message`，不匹配的行忽略）；每条诊断在存在"脚本源引用该文件"的实体时提供 Locate（按路径后缀双向匹配 `ScriptState.source_path`/path 形态的 inline source，命中最小实体 id 并调用 `document.select`，从不把文件附加到实体），无对应实体的错误不可点击。面板同时显示有界的原始输出尾部（4000 字符、按行对齐）。构建失败不致命；引擎在场景加载时读取并校验路径形式的脚本（`scene::load_script`），因此失败的构建也会以 "could not read scene script" 阻塞场景加载——面板状态说明这层关系。

## 视口编辑

- 视口交互运行在面板之间的中央空区（`ui_egui.rs::show_viewport`，接在 `Panels::show` 的面板接缝处）。场景铺满整个窗口、面板悬浮其上，因此 3D 投影按 egui 屏幕尺寸计算（矩阵约定逐项镜像 wgpu 渲染器的 `view_projection`）；精灵层按渲染器的 `world.viewport` 像素空间映射——精灵由 `shader.wgsl` 以像素空间绘制、不受相机影响，拾取与 gizmo 都镜像这一事实。纯数学部分在 `viewport.rs`（投影往返、拾取、对齐量化、相机操作），无渲染依赖、纯 f32/f64 确定性计算并有单元测试。
- 点击选择：视口内左键点击经 `viewport.rs::pick_entity` 选中实体，空白处清除选择；选择是文档状态，层级面板自动同步。精灵按渲染器的 `(layer, id)` 绘制顺序取最上层命中（精灵绘制在最后一层、无深度，命中优先于模型）；3D 模型按射线取最近命中——cube/plane 用轴对齐包围盒（忽略旋转的 AABB 近似），fbx 边界未知用变换位置的单位球近似。
- 移动 gizmo：单选实体时绘制三轴彩色箭头（X 红、Y 绿、Z 蓝），屏幕空间命中测试（14 点半径）抓取轴柄；拖拽把指针增量换算为沿轴位移（模型按投影轴段的像素长度换算世界单位，精灵直接按精灵空间增量），按对齐步长量化（默认 0.25；工具栏 Snap 开关与步长输入；关闭时按 1.0 整数对齐）。精灵实体只有 X/Y 两轴（引擎 2D 变换无 z，经 `SetTransform` 提交）；模型实体三轴（经 `SetModelTransform` 提交，旋转/缩放不动）。
- 拖拽提交语义：拖拽期间每帧经 `EditorDocument::preview_apply` 预览——不进撤销/重做历史，revision 与脏标记照常推进；松开时若最终变换不同于起始变换，经 `EditorDocument::record_external_edit` 记录恰好一条 "move entity" 历史步骤（撤销即还原起始变换）；若拖拽停在起点则精确还原且不记录任何历史。拖拽中途实体被删除或撤销时静默结束，不产生提交。
- 渲染网格：首个 UI 帧经 `RenderPluginApi::set_grid_visible(true)` 开启一次，工具栏 Grid 复选框随时开关；golden 模式不创建 UI，按构造不会触碰网格标志，捕获保持字节确定性。
- 鼠标相机操作：右键拖拽绕 `camera.target` 环绕（半径不变，俯仰钳制在极点之下），中键拖拽沿相机平面平移（抓取约定：内容跟随指针），与键盘相机路径一样经 `Command::SetCamera` 应用；gizmo 拖拽期间独占指针，不会同时环绕相机。

## 未保存更改与退出

- 文档脏（live revision 超过保存锚点）且不在播放中时：窗口关闭按钮与 Esc 触发 Save / Discard / Cancel 确认模态（普通 egui 窗口）。Save 先运行 `document.save()`，保存失败保持窗口打开并在错误行显示；只有 Discard、成功保存或文档干净才真正退出。播放中的关闭直接退出（播放状态按设计是瞬态、不可保存）。
- 文档脏时的场景切换（资产浏览器 Open、面板内打开工程）先走同一模态，再继续或放弃；播放期间的场景切换仍被文档的播放锁拒绝。

## 渲染与编辑管线

- 打开并校验 `.mnsc`。场景由 `document.rs` 的 `EditorDocument` 打开：撤销/重做历史、选择和脏标记是文档状态，所有编辑经会话命令 API 产生与 agent 编辑相同的 revision 和事件。
- 使用相同渲染和资源提供方显示 FBX/cooked 场景；方向键移动相机（egui 正在使用键盘时让位），PageUp/PageDown 缩放，Home 重置，右/中键拖拽环绕与平移（见"视口编辑"）。
- 显示实体层级、模型资源、材质、灯光和性能 overlay；overlay quad 仅在输入签名变化时重建，时间统计按 0.1 ms 量化，golden 模式始终安装空 overlay。字符 overlay 保持为诊断显示，与 egui 面板分开；overlay 中的场景路径跟随文档（经资产浏览器或工程切换后同步更新）。
- 播放模式（`document.rs`）：`enter_play` 捕获进入前的完整世界并启动 `core.simulation`，`exit_play` 停止提供方并经 `Engine::load_world` 恢复进入前的世界——revision 向前推进并发出 `WorldLoaded`（不用会回退 revision 的 `EngineSnapshot`），退出后文档按保守策略标记为脏。播放期间 `apply`、全部便捷编辑、撤销/重做与保存/另存为都明确拒绝（经典播放期编辑锁）；编辑帧驱动与窗口客户端一致的固定步进累加器（每帧至多 5 个逻辑帧，经 `RuntimeSession::step` 走插件宿主）；模拟失败退出播放模式并在错误行显示，不崩溃应用。脚本宿主随 `load_world` 清空并在下次播放重建（脚本堆状态从不快照）。
- 编辑帧经类型擦除的 `RenderPluginApi::set_egui_frame` 交给渲染插件（负载 `(Vec<egui::ClippedPrimitive>, egui::TexturesDelta, f32)`：egui 0.37 的分块细分必须在产生形状的上下文上运行，故由编辑器细分后移交；契约见 `crates/moni-core/src/host.rs`），由 wgpu 端的 egui-wgpu 合成器在呈现前作为最后一层绘制。
- golden 模式完全禁用 egui（不创建 egui 上下文、不发送任何帧，也不触发自动构建），保证 golden 捕获字节确定性；golden 模式也不响应播放快捷键。
- `Panels::show` 的最后一项是视口交互（拾取、gizmo、网格开关、鼠标相机操作），驱动同一文档 API；后续视口工具继续接入同一位置。

## 限制

没有停靠布局、动画、控件注册或插件 UI SDK。脚本只能被替换不能被移除（核心命令集没有脚本移除命令）：首次附加脚本撤销后脚本仍在；资产浏览器为扁平列表，不含缩略图、拖放或目录树。组件字段控件由 schema 属性类型驱动，不含按组件 ID 手写的专用界面。诊断到实体的 Locate 只做源路径后缀匹配：引用编译产物（如模板场景的 `build/main.js`）的实体不会认领 `.ts` 源文件的错误。视口内编辑目前只有移动 gizmo：旋转/缩放 gizmo 尚未实现；拾取按近似处理（cube/plane 忽略旋转的 AABB、fbx 单位球、忽略精灵旋转），精灵空间与 egui 坐标在默认 960x540 表面重合、窗口缩放后按比例映射。核心库保持 egui 无关；egui 类型只出现在本目录与渲染插件。
