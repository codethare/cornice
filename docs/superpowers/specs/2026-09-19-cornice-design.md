# cornice 设计文档

日期:2026-09-25(v7 — bar 改为直角)
状态:已确认(通知为 bar 外的独立卡片列;bar 固定为直角)

## 1. 目标

一个 Linux/Wayland 状态栏,用 Rust 写,单进程内同时提供 `org.freedesktop.Notifications` 通知守护进程。视觉目标:简介、优雅、一致。bar 与 swaybar / i3bar 一样使用完整直角矩形,不做胶囊或圆角端部。

通知不再插入 bar 的某个模块槽位,也不再从 bar 材质向下拉伸。通知显示在 bar 外侧下方,每条通知是一张独立卡片;多条通知按新 → 旧纵向排列,整列可以锚定在屏幕左上、中上或右上。卡片形状、间距、材质和动效参考 macOS 27 Golden Gate 的 Liquid Glass 设计语言,但使用 cornice 自己的比例和纯软件渲染能力。

### 非目标

- **tags / workspace / layout / mode / 聚焦窗口标题**(现代 river 不会向普通 bar 暴露这些状态,见 §2)
- 图标 / 图片通知(`icon-static`、`image-path`、主题图标查找、PNG 解码)
- `body-markup`、声音、DND 开关、通知历史中心
- GPU 渲染、实时模糊、折射滤镜或阴影
- 插件系统、布局语言、多个通知列
- macOS 品牌资产或逐像素复刻

## 2. 已验证的前提

### river 与 WM 的职责划分

- river 是非单体合成器:窗口管理由独立 WM 客户端通过 `river-window-management-v1` 实现。
- river 不向普通客户端暴露 `wlr-layer-shell-unstable-v1`。层壳客户端在 river 上能工作的前提是 WM 实现 `river-layer-shell-v1`。
- 目标运行时是 river(≥0.4.6)+ tailrace;tailrace 在焦点 output 上设置 layer-shell default。
- cornice 只使用标准 `wlr-layer-shell`。每张通知 surface 不指定 output,由 river + tailrace 选择当前焦点 output。
- `river-window-management-v1` 只发给单个 WM 客户端。第三方 bar 结构性无法获得现代 river 的窗口状态,因此不实现任何 `river.*` 模块。

来源:

- <https://codeberg.org/river/river>
- <https://github.com/codethare/tailrace>
- <https://codeberg.org/river/wiki/raw/branch/main/pages/useful-software.md>

### Apple 参考的边界

Apple 当前说明桌面通知出现在屏幕右上角,Notification Center 负责保留和分组历史通知;macOS 27 更新了 Liquid Glass 的可读性、圆角、材质层次和 spring 动效。Apple 没有公开 macOS 通知横幅的逐像素尺寸,因此本设计只采用可验证的设计原则,不把 Live Activity 尺寸误称为 macOS 通知规范。

来源:

- <https://support.apple.com/guide/mac-help/get-notifications-mchl2fb1258f/mac>
- <https://support.apple.com/guide/mac-help/notifications-settings-mh40583/mac>
- <https://developer.apple.com/videos/play/wwdc2026/289/>
- <https://developer.apple.com/documentation/technologyoverviews/adopting-liquid-glass>
- <https://developer.apple.com/design/human-interface-guidelines/notifications>

## 3. 架构

一个 `cornice` 进程,一条 calloop 主循环,一条独立 D-Bus 线程。

```
calloop 主循环
├── Wayland fd            (SCTK:registry、layer shell、shm、seat/pointer)
├── 帧定时器              (仅至少一张通知动画仍在进行时 arm,~60fps)
├── exec 子进程 channel
└── 1s 定时器             (clock 与通知到期)
        ▲
        │ calloop::channel
        │
D-Bus 线程 (zbus blocking, org.freedesktop.Notifications)
```

- 主循环没有 async;通知请求与 exec 输出都通过 `calloop::channel` 进入主线程。
- 每个 output 一张常驻 bar surface。
- 每个当前可见或正在退出的通知各有一张 `Overlay` layer surface。surface 独立拥有输入区域、命中矩形和动画状态。
- 通知 surface 创建时不指定 output,让 river + tailrace 路由到焦点 output;`wl_surface.enter` 记录实际 output,用于输出销毁后的重建。
- 所有通知 surface 在首个 `configure` 前不得 attach buffer。
- 静止时只有 1s 定时器;没有通知动画时不 arm 帧源。

### Surface 几何

| surface | layer | anchor | 生命周期 |
|---|---|---|---|
| bar | `Top` | top + left + right | 常驻,每 output 一个 |
| 通知 | `Overlay` | left: top + left;center: top;right: top + right | 每条通知一个;入场时创建,退场完成后销毁 |

协议规定只给一个轴的锚点时,该轴居中。因此 `position = "center"` 使用 `Anchor::TOP`,不计算虚构的 output 中心坐标。

通知列第一张卡的上边距为:

```
top_margin = bar.margin + bar.height + card_gap
```

左/右列的屏幕边距为 `2 × card_gap`;中列由 compositor 居中。通知不占用 exclusive zone。

## 4. 配置

位置 `$XDG_CONFIG_HOME/cornice/config.toml`,TOML + serde。解析失败时向 stderr 打印含 `line:column` 的错误并退出;不猜测、不静默降级。文件缺失不是错误。

```toml
[bar]
height   = 30
margin   = 0
padding  = 8
spacing  = 6

[theme]
background = "#1a1a1aee"
foreground = "#dcdcdc"
accent     = "#88c0d0"
font       = "Inter 11"

[bar.left]
modules = [ { kind = "clock", format = "%H:%M" } ]

[bar.center]
modules = []

[bar.right]
modules = [
  { kind = "exec", command = "while true; do cat /sys/class/power_supply/BAT0/capacity; sleep 5; done", format = "{out}%" },
]

[notification]
position     = "right" # left | center | right
max_visible  = 4
enter_ms     = 220
exit_ms      = 160
```

bar 没有圆角配置:背景直接用 `fill_rect` 铺满 surface,左右内容只服从 `bar.padding`。旧的 `theme.radius` 会被未知字段校验拒绝,不会静默忽略。

`[notification]` 是通知系统的开关:

- 整个 section 缺失时,D-Bus 守护进程仍运行并处理/到期通知,但不创建通知 surface。
- section 存在时,`position` 默认 `right`,与 macOS 桌面通知习惯一致。
- `position` 只接受 `left`、`center`、`right`;其他值在解析层报错并带位置。
- 旧的 `{ kind = "notification" }` bar 模块被删除,不再兼容;它会作为未知 module kind 报错。

bar 的三个 module 列表只包含 `clock` 与 `exec`。通知卡片是否存在、位于哪里,完全由 `[notification]` 决定。

## 5. bar 契约

```rust
struct Span {
    text: String,
    color: Option<Color>,
    bg: Option<Color>,
    action: Option<Action>,
}

trait Module {
    fn update(&mut self, ev: &Event) -> bool;
    fn spans(&self) -> Vec<Span>;
}
```

- bar 背景始终是直角矩形;圆角只属于通知卡片与 action 胶囊。
- left 靠左排,right 靠右排,center 以屏幕中线居中;两端只使用 `bar.padding`,没有圆角 optical inset。
- 三者重叠时 center 优先,两侧模块按可用宽度截断。
- 模块之间使用 `bar.spacing`;没有通知模块,因此通知出现和消失不会改变 bar 内任何模块的位置。
- 文本按 cap height 定位,字体度量由首帧光学校正并缓存。

## 6. 通知语义

### D-Bus 接口

- `GetCapabilities` → `["body", "actions", "persistence"]`。
- `Notify(app_name, replaces_id, app_icon, summary, body, actions, hints, expire_timeout) -> id`
- `CloseNotification(id)`
- `GetServerInformation` → name `cornice`,spec 1.2
- 信号:`NotificationClosed(id, reason)`、`ActionInvoked(id, action_key)`

### 队列

- 可见顺序为新 → 旧;`max_visible` 之外保留但不绘制。
- 容量为 `max_visible × 4`;溢出时静默丢弃最旧条目。
- `replaces_id` 命中时就地替换,不重放入场动画。
- `expire_timeout = -1` 使用 low 4s / normal 6s / critical 不自动消失;`0` 永不自动消失;`>0` 照用。
- 关闭原因:1 = timeout,2 = 用户,3 = `CloseNotification`。
- D-Bus 不可用时 bar 继续运行,仅通知功能不可用。

### 不可信内容

- `app_name`、summary、action label 只取第一行并按卡片内容宽裁剪。
- body 最多 5 行 / 300 字符,每个 body 行按内容宽裁剪。
- 多行 body 和 actions 参与卡片高度计算。
- action 命中矩形不得越过卡片 inner edge;只绘制能够完整容纳的 action。
- 点击卡片主体或中键关闭;左键 action 先于卡片主体命中。

## 7. 独立卡片布局与材质

### 比例

通知的所有固定尺寸都从 `theme.height` 派生:

```text
card_gap     = max(height / 5, 2)
card_padding = max(height / 2, 4)
card_w       = clamp(10 × height, 80, 420)
card_radius  = 1.2 × height
card_min_h   = max(2.5 × height, text line + 2 × card_padding)
```

- 宽度固定,同一列中的卡片等宽;高度按内容增长,最低为 `card_min_h`。
- 内容在卡片内垂直居中;标题与来源组成第一行,正文/actions 作为 detail block。
- detail block 与标题间使用 `card_gap / 2` 和 `max(card_gap / 3, 1)` 的低对比度分隔线。
- 卡片背景使用 `theme.background` 的半透明色,所有圆角使用 `geom::CORNER_EXPONENT = 4` 的连续曲线。
- 卡片与 bar 没有重叠,因此不画“接在 bar 底边下面”的特例,也不画第二层背景。
- 不画 GPU 模糊或阴影;透明叠加和内容层级承担 macOS 27 的轻盈感。

### 列布局

`visible` 中每条通知都生成一个 `Card`:

```text
card[0].top = 0
card[i+1].top = card[i].top + card[i].height + card_gap
```

`position` 只改变每张 layer surface 的水平 anchor,不会改变内容布局。三种位置都必须让整张卡和其命中区域保持在对应一侧。

## 8. 动画

通知动画状态按通知 id 保存;不同通知可以同时播放,但整个进程仍只有一个 1s 定时器和一个 frame source。

- 入场:新卡从 `scale = 0.94`,`alpha = 0` 到 `scale = 1`,`alpha = 1`;使用 `Easing::Spring`,配置 `enter_ms`。
- 退场:卡从当前形状到 `scale = 0.96`,`alpha = 0`;使用 `Easing::Smooth`,不超调,配置 `exit_ms`。
- 旧卡重新排布:每张仍存活的卡从旧 `top` spring 到新 `top`;新卡出现和旧卡关闭都会触发。
- 文本随卡片一起淡入淡出;不对每行文字单独做位移动画,避免内容抖动。
- 退场中的卡不响应点击,输入区域立即清空。
- 任一路径开始或延长动画都必须调用 `ensure_frame_source()`;动画完成后清除 `animating`,frame source 用 `TimeoutAction::Drop` 自毁。
- `replaces_id` 只更新内容并重新计算必要的高度/位置,不重放入场。

动画的纯逻辑(位置/比例/alpha/完成判断)放在 `notify::view`;Wayland 只拥有 surface、buffer、timer 和协议回调。

## 9. 输入与生命周期

- 绘制和 `set_input_region` 使用同一份每卡命中矩形;`set_input_region` 永远传 `Some`,透明或退场状态传空 region。
- 一个通知的 action 矩形先于该通知的主体矩形;不同 surface 之间由 Wayland surface 命中路由。
- 点击 action 发出 `ActionInvoked` 后关闭;点击主体发出 `NotificationClosed(..., 2)`。
- 队列移除后,退场通知暂时保留自己的 `Notification` 副本和 surface,直到 exit tween 完成;其他卡同时向新的列位置补位。
- compositor 关闭通知 surface 时不退出进程;清掉该 surface 后按当前队列重建。
- 输出销毁时,落在该输出的通知 surface 被销毁并在新的默认 output 上重建。

## 10. 文件职责

| 文件 | 责任 |
|---|---|
| `geom.rs` | `Rect` / `Color`;alpha 透传,只在 `to_shm_bytes` 预乘 |
| `canvas.rs` | `wl_shm` 写入、裁剪、连续圆角;不知道文字 |
| `text.rs` | cosmic-text、宽度裁剪;不知道布局 |
| `widget.rs` | `Span` / `Action` / `Event` / `Module` |
| `theme.rs` | 颜色与由 height 派生的比例 |
| `config.rs` | TOML schema、`[notification].position`、解析与行号错误 |
| `anim.rs` | `Easing` / `Tween`;不持有协议或布局 |
| `bar/mod.rs` | 左/中/右布局;不绘制通知 |
| `bar/modules.rs` | `clock` / `exec` |
| `notify/queue.rs` | 通知状态机;不碰 D-Bus 或渲染 |
| `notify/service.rs` | zbus 接口和信号 |
| `notify/view.rs` | 独立卡片尺寸、列位置、动画插值、绘制和命中 |
| `wayland/mod.rs` | 唯一持有 `State` 和 Wayland 回调;每卡 surface 生命周期与 timer |

## 11. 测试与手动验证

### 纯逻辑 `cargo test`

必须覆盖:

- `[notification]` 缺失时关闭,三种 position 解析,非法 position 的行号
- bar module 只接受 `clock` / `exec`,通知出现不改变 bar 布局
- 多条通知全部生成独立卡片,顺序、间距、宽度和最小高度
- 三种锚点的 top margin 与 side margin
- 不可信字段/body/action 裁剪,action 命中矩形不越界
- 入场/退场端点、alpha/scale 单调性、完成判断、逐卡独立性
- 队列替换、可见窗口、容量、超时、关闭原因
- `Rect` / canvas / 文本既有纯逻辑回归

协议和像素层不做自动化断言,按 `docs/smoke.md` 手动检查。

### Smoke 重点

- bar 四角为完整直角,没有 capsule 端部或透明圆角;第一条通知出现时 bar 文本不移动。
- 单条通知是一张完整独立卡,不是 bar 的延伸;短通知也不与 bar 拼接。
- 多条通知新 → 旧纵向排列,每张有独立背景、连续圆角、间距和点击区域。
- `left` / `center` / `right` 三种位置整列对齐正确,透明区域点击穿透。
- 入场和退场 spring 可见且不造成文本跳闪;关闭一条后其余卡平滑补位。
- 退场/透明区域不吞点击;action 优先于主体关闭。
- 首个 configure 前没有 buffer,没有 `wl_surface` 协议错误。
- river + tailrace 下每张通知路由到焦点 output,输出移除后能重建。

## 12. 风险

- 开发环境没有可用的 river 会话,只能保证纯逻辑测试和构建;动画手感与真实 layer-shell 行为必须在目标环境 smoke。
- `Anchor::TOP` 的水平居中由 layer-shell 协议定义,但 river 的 WM 转发实现仍需手动确认。
- 纯软件逐卡 surface 会增加 Wayland surface 数量,上限由 `max_visible` 和队列容量约束;不做 GPU 模糊。
- 内容高度依赖 cosmic-text 字体度量,配置字体变化会改变卡片高度;smoke 需覆盖 fallback 字体。

## 13. 实施顺序

1. 配置 schema 与 bar 模块删除,先让旧 bar 编译并通过纯逻辑测试。
2. `notify::view` 独立卡片布局和逐卡动画纯逻辑。
3. Wayland 每卡 surface、anchor、输入与 timer 接线。
4. 更新模板、smoke 和本设计约束。
5. `cargo fmt --check`,`cargo test`,`cargo build`。
