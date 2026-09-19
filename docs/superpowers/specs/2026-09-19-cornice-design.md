# cornice 设计文档

日期:2026-09-19(v2 — v1 的 `river.*` 模块已删除,理由见 §2)
状态:已确认,待写实施计划

## 1. 目标

一个 Linux/Wayland 状态栏,用 Rust 写,单进程内同时提供 `org.freedesktop.Notifications` 通知守护进程。视觉目标:简介、优雅、一致。通知以"从右侧栏组件变形生长出来"的方式出现在右上角,模仿 iOS 灵动岛。

首要目标是 river 窗口管理器上的可用性,但代码不包含任何 river 专有分支。

### 非目标(v1 明确不做)

- **tags / workspace / layout / mode / 聚焦窗口标题**(结构上不可得,见 §2)
- 图标 / 图片通知(`icon-static`、`image-path`、hicolor 主题查找、PNG 解码)
- `body-markup` 解析,以及声音
- DND 开关、通知历史中心
- 点击标签切换 workspace
- GPU 渲染、模糊/阴影过渡
- 插件系统、脚本语言的配置、布局语言

## 2. 已验证的前提

### river 与 WM 的职责划分

- river 是**非单体**合成器:窗口管理由独立进程(WM)通过 `river-window-management-v1` 实现。
- river **不向普通客户端暴露** `wlr-layer-shell-unstable-v1`。层壳客户端在 river 上能工作,前提是 **WM** 实现了 `river-layer-shell-v1`(由 WM 声明"我来处理层壳")。
- river 官方 wiki 的软件列表对 mako / fuzzel / swaybg / eww 等逐条标注 "require the window manager to implement the river-layer-shell-v1 protocol"。

### 目标 WM:tailrace

- 目标运行时是 **river(最新,≥0.4.6)+ tailrace**(<https://github.com/codethare/tailrace>),不是 kwm。
- tailrace **实现了 `river-layer-shell-v1`**:vendored `protocol/river-layer-shell-v1.xml`,`src/seat.rs` 处理 exclusive / non-exclusive layer-shell 焦点。
- tailrace 源码中**没有任何 wl_shm 或渲染代码** —— 层壳 surface 由 **river 自己合成**,WM 只负责焦点与策略。
- 结论:cornice 作为普通 `wlr-layer-shell` 客户端,在 river + tailrace 上可正常出图,不依赖 WM 的渲染能力。

### 为什么没有 `river.*` 模块

v1 的 spec 里计划了 `river.tags` / `river.title` / `river.layout` / `river.mode`,依赖 `river-status-unstable-v1`。该协议**只存在于 river-classic**,当前 river 里已经没有了:

1. river 的 `protocol/` 目录只有:`river-input-management-v1`、`river-layer-shell-v1`、`river-libinput-config-v1`、`river-touch-gestures-v1`、`river-window-management-v1`、`river-xkb-bindings-v1`、`river-xkb-config-v1`。
2. waybar 的 `river/tags` 在新 river 上直接报 `river_status_manager_v1 not advertised`。
3. `ext-workspace-v1` 是尚未实现的 feature request(river#1402,2026-03 开)。
4. tailrace 的 `protocol/` 里同样没有状态协议,也不发布任何 workspace 状态。
5. `river-window-management-v1` 按协议规定只发给 WM 一个客户端,bar 拿不到 tags。
6. river 支持 `ext-foreign-toplevel-list-v1`(0.3.13 起),但只提供 title / app_id,**没有 focus 信息**。

即:**在现代 river 上,tags / 当前窗口标题结构上无法被第三方 bar 获取。** 未来的路只有两条,都不在本项目范围内:上游 river 实现 `ext-workspace-v1`,或 tailrace 自己发布一个状态协议。

来源:
- <https://codeberg.org/river/river> (protocol/ 目录、README)
- <https://github.com/codethare/tailrace> (protocol/、src/)
- <https://codeberg.org/river/wiki/raw/branch/main/pages/useful-software.md>
- <https://codeberg.org/river/river/issues/1402>

## 3. 架构

一个进程 `cornice`,一条事件循环,三种职责。

```
calloop 主循环
├── Wayland fd            (SCTK:registry、layer shell、shm、seat/pointer)
├── 帧定时器              (仅动画进行中 arm,~60fps)
├── 子进程 channel        (exec 模块 stdout 行)
└── 定时器                (clock)
        ▲
        │ mpsc (calloop::channel)
        │
D-Bus 线程 (zbus blocking, org.freedesktop.Notifications)
```

- D-Bus 在独立线程使用 zbus 的 **blocking** API(它自带执行器线程),接口方法把请求结构体通过 `calloop::channel::Sender` 投给主循环。**主循环内没有 async,状态集中在单线程**,动画与通知队列无需加锁。
- 每个 output 拥有一套(栏 surface + 通知 surface),同进程内多实例共存。

### Surface

| surface | layer | anchor | 生命周期 |
|---|---|---|---|
| 栏 | `Top` | top + left + right | 常驻,每 output 一个,`exclusive_zone = 栏高 + 2×margin` |
| 通知 | `Overlay` | top + left + right,`margin_top = bar.margin` | 队列非空时创建,清空后销毁 |

通知 surface 是全宽透明大块(高度 = 栏高 + 卡片区),所有卡片绘制在同一个 buffer 内。每帧调用 `set_input_region` 设为其可见形状的并集,因此透明区域对指针完全穿透。它铺在栏之上(`Overlay` > `Top`),这是"从右侧组件长出来"成立的前提:t=0 时岛的矩形与栏右簇的矩形像素重合。

### 渲染栈

`smithay-client-toolkit` + `wl_shm` + `cosmic-text`(内部 swash)。纯软件渲染,无 GPU。

理由:本设计需要的动画是布局插值(位置/尺寸/圆角/颜色),不是特效,软件渲染可逐帧精确控制;横条 1920×32 ≈ 6 万像素、通知卡片 ≈ 5 万像素,60fps 重绘对 CPU 无压力。cosmic-text 提供文本整形与字体回退,保证中文/emoji 不出现乱码 —— 对一个中文用户日常贴顶的横条是硬需求。wl_shm 也是唯一被上游广泛验证的层壳客户端路径。

排除项:`wgpu/glow`(依赖与复杂度翻倍,对扁平风格无收益)、`iced/egui/gtk-layer-shell`(控件风格不可控,与"一致"目标冲突,打包体积大)。

## 4. 配置

位置 `$XDG_CONFIG_HOME/cornice/config.toml`,TOML + serde。解析失败时向 stderr 打印含行号的人类可读错误并退出非零 —— 不猜、不静默降级。

```toml
[bar]
height   = 30
margin   = 0        # 栏到屏幕边缘
padding  = 8        # 栏内左右留白
spacing  = 6        # 模块之间

[theme]
background = "#1a1a1aee"
foreground = "#dcdcdc"
accent     = "#88c0d0"
font       = "Inter 11"
radius     = 15     # 缺省 = height/2,即胶囊形

[bar.left]
modules = [ { kind = "clock", format = "%H:%M" } ]

[bar.center]
modules = []

[bar.right]
modules = [
  { kind = "exec", command = "while true; do cat /sys/class/power_supply/BAT0/capacity; sleep 5; done", format = "{out}%" },
  { kind = "exec", command = "while true; do awk '{print int($1/1024)}' /proc/loadavg; sleep 2; done", format = "L{out}" },
]

[notification]
max_visible = 4     # 超出者保留在队列中不绘制
enter_ms    = 220   # 进场(岛 → 卡片)
exit_ms     = 160   # 退场(卡片 → 岛)
```

三块各自是一个**有序模块列表**,这就是"自由搭配"的全部含义:没有嵌套容器,没有布局语言。换顺序即换顺序,关掉即删掉。

**比例派生**:`radius`、卡片间距、卡片内边距缺省都由 `height` 派生,需要时再显式覆盖。默认值下,一屏配置只需写三个模块列表 —— 这是"一致"的来源。

### 模块种类(v1)

| kind | 数据来源 | 参数 |
|---|---|---|
| `clock` | 本地时间 | `format`(chrono 格式串) |
| `exec` | 长驻子进程 stdout,一行一次更新;进程退出后 1s 重启 | `command`、`format`(含 `{out}`) |

只有两种,而且是刻意的:现代 river 上第三方 bar 拿不到 WM 状态(§2),系统信息(CPU/内存/电量)本来就该由脚本产出,这也正是最初需求里说的"支持由外部的脚本来显示 CPU/mem/battery"。

`exec` 采用长驻流式(而非定时轮询):轮询类需求由脚本自己写循环表达(`while true; do …; sleep 5; done`),bar 端因此不需要 interval 机制与进程 spawn 调度。

## 5. 模块契约

```rust
struct Span {
    text: String,
    color: Option<Color>,
    bg: Option<Color>,
    action: Option<Action>,
}

trait Module {
    fn update(&mut self, ev: &Event) -> bool; // 返回是否需要重绘
    fn spans(&self) -> Vec<Span>;
}
```

`Span` 是唯一的 widget 原语,同时覆盖:多段异色文本、通知按钮、模块文字。栏渲染即把各模块的 spans 展平、排版、记录命中矩形。v1 只把 `action` 接到通知按钮上 —— 原语保留,依赖不引。

### 布局算法

- left 靠左排,right 靠右排,center 以**屏幕中线**居中(不是以剩余空间居中)。
- 三者重叠时 center 优先,两侧模块按需省略号截断。
- 规则写进文档,保证可预测:不会因为文字变长而抖动。

## 6. 通知子系统

### D-Bus 接口

- `GetCapabilities` → `["body", "actions", "persistence"]`。不声明 `body-markup` / `icon-static` / `sound`,客户端(含 `notify-send`)会自动退化为纯文本。
- `Notify(app_name, replaces_id, app_icon, summary, body, actions, hints, expire_timeout) -> id`
- `CloseNotification(id)`
- `GetServerInformation` → name `cornice`,spec 1.2
- 信号:`NotificationClosed(id, reason)`、`ActionInvoked(id, action_key)`

### 语义

- **hints**:只认 `urgency`(0/1/2,决定配色与缺省时长);其余忽略且不报错。
- **时长**:`expire_timeout = -1` 用缺省(low 4s / normal 6s / critical 不自动消失);`0` 永不自动消失但仍可点击关闭;`>0` 照用。
- **`replaces_id`**:命中已有条目则就地替换内容(不重放形变),否则新建。
- **截断**:正文超长按行数与字符数截断并加省略号。这是信任边界,必须做。
- **队列**:按新→旧排序,`max_visible` 之外的不绘制但保留,前方消失后依次滑入;溢出丢弃最旧。
- **关闭原因**:1 = 超时,2 = 用户关闭,3 = `CloseNotification`。

### 输入

左键点卡片 = 关闭;中键 = 关闭(mako 惯例);左键点按钮 = `ActionInvoked` + 关闭。命中判定与 `set_input_region` 共用同一份绘制期产出的矩形表,避免两处不一致。

### 降级

若 `org.freedesktop.Notifications` 已被其他守护进程占用,栏照常运行,打印警告,仅通知功能不可用 —— 不退出。

## 7. 岛屿形变

单条 tween,插值对象只有五项:rect、radius、配色、文字 alpha。

```
t=0   卡片矩形 == 右簇矩形,圆角 = 栏圆角,底色 = 栏底色   → 与栏像素完全重合,视觉上"不存在"
t=1   卡片矩形 == 右上角最终卡片位置,圆角 = 卡片圆角,底色 = 卡片底色
```

- x/y/w/h/radius/color 插值。
- 文字立刻按最终宽度排版,但裁剪到卡片矩形内,alpha = `clamp((t - 0.35) / 0.65)`。效果即"胶囊张开成盒子,字随后浮出"。
- 进场 220ms ease-out-cubic;退场 160ms,反向回缩到右簇。两者缺省值可在 `[notification]` 覆盖。
- 动画期间栏 surface 完全不重绘,只有 overlay surface 在动。
- 静止时不 arm 帧定时器,零唤醒。

**关键耦合**:t=0 的矩形必须严格等于右区块全部模块(含模块间距)的整体矩形,外扩 `bar.padding`,y 为 `bar.margin`,高为 `bar.height`。因此栏的布局结果(`BarLayout` 快照)就是通知模块的输入 —— 同一份数据,不重算。

**退化情况**:未配置右侧模块时,岛从栏右边缘一个点(宽 0)长起,高度取栏高。

**多显示器**:通知投给当前 focus 的 output,使用该 output 的栏矩形。

## 8. 文件划分

```
Cargo.toml
src/main.rs                       装配:config → wayland → surfaces → channels
src/geom.rs                       Rect、Color
src/config.rs                     serde schema、默认值派生、校验与错误文案
src/theme.rs                      配色 + 由 height 派生的比例
src/widget.rs                     Span / Action / Module / Event
src/canvas.rs                     圆角矩形、裁剪、alpha 混合
src/text.rs                       cosmic-text 排版与光栅化、省略号截断
src/anim.rs                       easing + Tween
src/wayland/mod.rs                连接、registry、layer surface、shm 缓冲池、seat
src/bar/mod.rs                    栏 surface、左中右布局、BarLayout 快照
src/bar/modules.rs                clock / exec
src/notify/queue.rs               通知队列状态机(纯逻辑)
src/notify/service.rs             zbus 线程 + D-Bus 方法
src/notify/view.rs                卡片排版 + 岛屿 tween + 命中
docs/smoke.md                     手动验证清单
```

比 v1 spec 多出 `geom.rs` / `widget.rs` / `notify/queue.rs`(bar 与 notify 共用的原语,以及可单测的状态机),少掉 `build.rs` 与 `protocols/`(不再需要任何自定义协议)。

## 9. 测试

全部为可运行的纯逻辑测试,不引入测试框架:

- **配置**:解析、缺省值派生(如 `radius = height/2`)、错误信息含行号
- **几何**:`Rect::contains` / `union`(输入区并集)、`Canvas` 越界不 panic
- **canvas**:圆角矩形四角不被填充、中心被填充、alpha 混合结果
- **文字**:按宽度截断加省略号(注入假测量函数)
- **exec**:`{out}` 模板替换、子进程退出后重启
- **tween**:t=0 等于起点、t=1 等于终点、进度单调、超出范围被 clamp
- **通知状态机**:时长缺省表(-1 / 0 / urgency)、`replaces_id` 就地替换、`max_visible` 溢出、关闭原因 1/2/3、超长截断
- **命中**:按钮矩形 ↔ action 映射

协议层不做自动化测试(需要真实 compositor),改为 `docs/smoke.md` 中的手动清单。

## 10. 里程碑

每个里程碑都能单独跑起来:

1. Wayland 连接 + layer-shell 栏 surface + shm + 纯色背景(端到端探针)
2. 配置 schema + 主题派生 + 校验
3. Canvas + 文字排版
4. 栏左中右布局 + `clock` 模块
5. `exec` 模块(子进程、行协议、重启)
6. `anim`(easing / Tween)
7. 通知服务:D-Bus + 队列状态机
8. 通知视图:静态卡片 + 输入命中
9. 岛屿 tween 接线 + `max_visible` + `docs/smoke.md`

## 11. 风险

- **开发沙箱内没有可用的 river 会话。** 构建环境只能保证编译通过 + 纯逻辑测试通过;实际画面、层壳行为、动画手感必须在使用者的 river + tailrace 会话中验证。不得声称已验证未验证的内容。
- 沙箱内 `XDG_RUNTIME_DIR` 缺失,任何需要真实 session bus / wayland socket 的步骤都无法在此运行。
- **`river-layer-shell-v1` 由 WM 转发**,与 wlroots 原版可能存在细节差异,尤其涉及运行时改尺寸、`set_input_region`、overlay layer 的堆叠。因此里程碑 1 即最小端到端探针,让风险最先暴露。
- 若 tailrace 未绑定 `river_layer_shell_v1`(旧版本,或配置异常),cornice 会在启动时报 "layer shell is not available" 并退出 —— 这是预期行为,不是 bug。
