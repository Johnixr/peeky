# peeky

[English](./README.md) · 简体中文

> 一只透明悬浮的 **桌面 AI 小精灵**（macOS / Windows 11）——探头看你的屏幕，
> 在恰当的时机开口。

peeky 用 [Tauri 2](https://tauri.app) 构建：Rust 后端（`src-tauri/`）+ 原生
TypeScript + Vite 的 webview 前端（`src/`）。它是一只可拖拽、无窗口边框的小角色，
停在桌面任意位置，周期性地感知你屏幕上的内容，**在该说的时候说，其余时候保持安静**。
同时提供即时的**截图 → 提问 / 讲解 / 翻译**快捷键。

核心理念：AI 不该只在你打开聊天框、敲下提示词时才出现。它该像朋友一样坐在你旁边，
留意你在做什么，在真正有用的时候搭把手。

> 平台：**macOS**（Apple Silicon / Intel）和 **Windows 11**。

## 功能

- **透明悬浮精灵** —— 纯程序化内联 SVG 角色（无需贴图素材），可拖到任意位置、始终置顶、
  除精灵本体外点击穿透。结果卡片以**流式 Markdown** 渲染。
- **感知 → 适时开口循环** —— 500ms 后台循环截图，先跑廉价的 pHash + 滚动变化检测
  （不调模型、不做 OCR），防抖之后才在确有意义的变化时调用模型。
- **人格模式** —— `段子手` / `博学` / `副驾`（`Ctrl+Shift+M` 切换），
  prompt 在 `src-tauri/prompts/*.md`。
- **快捷截图功能** —— 冻屏后用放大镜精确框选一块区域，然后：
  - `Ctrl+Shift+E` —— **讲解**所选内容
  - `Ctrl+Shift+B` —— 对它**提问**（输入文字）
  - `Ctrl+Shift+T` —— **翻译** + 简短生词讲解
- **克制引擎** —— 每小时发言预算、安静时段、跟随系统专注 / 勿扰、全屏自动暂停——
  让 peeky 永远不会变成"大眼夹"。
- **OpenAI 兼容流式** —— 任意 `/chat/completions` 端点（云端 / 私有 / 本地）。
  视觉消息、SSE 流式、Token 计量。**绝不关闭 TLS 校验，绝不硬编码密钥。**
- **推理强度控制** —— 关闭 / 低 / 中 / 高，在推理类模型上以速度换质量
  （StepFun、GPT-5/o 系列、Qwen、DeepSeek 等）。
- **多语言** —— 完整 **中 / 日 / 英** 界面与回复；默认跟随系统区域，可在设置切换。

## 运行

前置：

- **macOS**：macOS 11+、[Rust](https://rustup.rs)（stable）、[pnpm](https://pnpm.io)、
  Xcode 命令行工具。
- **Windows 11**：[Rust](https://rustup.rs)（stable，≥ 1.85）、[pnpm](https://pnpm.io)、
  Visual Studio Build Tools（C++ 工作负载）、WebView2 运行时（Windows 11 自带）。

```sh
pnpm install
pnpm tauri dev      # 开发构建（debug 下图像处理较慢）
pnpm tauri build    # 生产 .app / .dmg  （macOS）·  .exe / NSIS / .msi  （Windows）
```

### macOS 权限

在 **系统设置 → 隐私与安全性** 中给 peeky 授予 **屏幕录制**（必需，这是核心）
和 **辅助功能**（用于窗口上下文 + 副驾输入），然后**退出并重新打开**。

> 注意：macOS 对 ad-hoc 临时签名的屏幕录制授权很挑剔——在 macOS 15 上，ad-hoc
> 构建可能"已授权"却仍截到黑屏。请用正式的 Apple 开发者签名构建，授权才会在重新
> 构建后依然有效：`APPLE_SIGNING_IDENTITY="<你的签名>" pnpm tauri build`。

### Windows 权限

Windows **没有 per-app 的"屏幕录制"开关**——peeky 第一次截图时会弹出**系统级**的
捕获授权框，同意一次后即可。如果偶尔截到空白画面，请完全退出并重新打开 peeky
（部分显卡在 DWM 下的兼容问题）。

### 配置模型

打开设置（悬停点齿轮，或 `Ctrl+Shift+S`），填写 **Base URL**、**API Key**、
**模型**（例如 `https://api.stepfun.com/v1` / `step-3.7-flash`）。
密钥也可通过环境变量 `PEEKY_API_KEY` 提供（见 `.env.example`）——**密钥绝不入库**。
用 **测试连接** 验证。

### 快捷键

| 快捷键 | 动作 |
| --- | --- |
| `Ctrl+Shift+Space` | 手动触发（截图 + 开口） |
| `Ctrl+Shift+E` / `B` / `T` | 框选 → 讲解 / 提问 / 翻译 |
| `Ctrl+Shift+M` | 切换人格模式 |
| `Ctrl+Shift+P` | 暂停 / 恢复 |
| `Ctrl+Shift+S` | 打开设置 |

单击精灵切换卡片；双击暂停；右键手动触发。

## 项目结构

- `src-tauri/` —— Rust 后端（截图、触发、API、模式、克制、记忆、工具、权限、命令；
  主循环在 `lib.rs`）。
- `src/` —— TypeScript 前端（精灵、框选层、设置、i18n、事件粘合）。
- `src-tauri/prompts/` —— 各模式 + 快捷功能的系统提示。
- `ARCHITECTURE.md` —— 更深入的设计说明 · `CLAUDE.md` —— 贡献者 / agent 指南。

## 许可证

[MIT](./LICENSE)。
