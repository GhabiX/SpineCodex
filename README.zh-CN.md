<h1 align="center"><img src="./.github/assets/spinecodex-tree.svg" width="56" alt="SpineCodex 树形标志" /> SpineCodex</h1>

<p align="center"><em>让你的 Codex 在 SpineTree 上工作、演化与扩展。</em></p>

<p align="center"><a href="https://www.npmjs.com/package/@spinejit/spine-codex"><img src="https://img.shields.io/npm/v/%40spinejit%2Fspine-codex?label=npm" alt="npm 版本" /></a> · <a href="./LICENSE"><img src="https://img.shields.io/badge/license-Apache--2.0-blue.svg" alt="Apache-2.0 许可证" /></a></p>

<p align="center"><a href="./README.md">English</a> · 简体中文</p>

<p align="center">
  <img src="./.github/assets/spinecodex-tui.gif" width="800" alt="SpineCodex TUI 演示" />
</p>

## 为什么选择 SpineCodex

SpineCodex 让你的 Codex **在一棵 SpineTree 上工作**：长周期、多步骤工作会被拆分为有明确边界的 Work Unit，由 Runtime 持久化为 SpineBranch，并随着树的演化不断细化、委派和完成，而不必把整个过程不断堆进一条越来越长的 transcript。

### 快速开始

在现有 Codex 环境中安装并直接运行。当前开发分支对齐上游 OpenAI Codex `0.153.4`；已发布版本见[发布记录](https://github.com/GhabiX/SpineCodex/releases)：

```bash
npm install -g @spinejit/spine-codex@latest
spine-codex
```

Spine Spawn 默认开启。运行 `/experimental` 启用可选的 Memory Projection，保存设置后开始新的对话。在 `~/.codex/config.toml` 中设置 `spine_spawn.max_concurrent_threads_per_session`，即可配置每个会话的总线程上限（含根线程）。

### 带来的能力

与 Codex 相比，SpineCodex 在 [SWE-Milestone](https://github.com/DeepCommit-ai/SWE-Milestone) 上以低 27% 的总成本多解决 **89% 的任务**，并将有效工作上下文最多扩展至 **10 倍**。它还在 [ProgramBench](https://programbench.com) 上将平均得分提高 **10.8 分**，并在 [FrontierSWE](https://www.frontierswe.com) 上将平均得分提高 **9.2 分**。

| 线性上下文                        | SpineCodex                                                                                                                          |
| --------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------- |
| ❌ **上下文不够用？**             | ✅ **256K → 2.5M 有效工作上下文**<br />SpineJIT 将已完成分支编译为语义化节点记忆（Node Memory），使有效工作上下文突破原生窗口限制。 |
| ❌ **反复压缩后发生偏移？**       | ✅ **最小有效上下文，最大专注度。**<br />Spine Runtime 维护 SpineTree，并只为当前 Work Unit 投影所需上下文，让智能体保持专注。              |
| ❌ **在长任务中失去耐心与专注？** | ✅ **按需递归扩展子智能体。**<br />SpineJIT 让智能体能够按需递归展开为专门的子智能体，为复杂问题引入分治结构和更深的推理。          |

## 更新内容

### 即将推出

- **PI 和 DeepSeek Harness 插件** — 基于 SpineSDK 的集成正在开发中。
- **App Spine UI** — 支持在 Codex App 中查看和操作 SpineTree、Work Unit 及运行时状态。正在开发中。

<details>
<summary>预览 App Spine UI</summary>

  <p align="center">
    <img src="./.github/assets/spinecodex-app-ui-preview.gif" width="820" alt="即将推出的 App Spine UI 预览" />
    <br />
    <sub>预览：App Spine UI 正在开发中。</sub>
  </p>
</details>

<details>
<summary>版本</summary>

<details>
<summary>0.4.0</summary>

将上游 Codex 基线更新到 `0.153.4`。解开源账本历史条数与单轮可见 item 上限的错误绑定，长会话不再在 4096 条原始历史上被打死。
</details>

<details>
<summary>0.3.3</summary>

改进 paginated 会话恢复：不兼容的历史记录不再阻塞有效 lineage，同时文件损坏和 lineage 边界错误仍会正常失败；增加异常限流记录 replay 的回归覆盖。
</details>

<details>
<summary>0.3.2</summary>

恢复上游 Codex 兼容身份（`0.147.0`），同时保持 SpineCodex 产品版本独立；并加强更新缓存隔离、恢复后的 Spawn 可见性和发布元数据检查。
</details>

<details>
<summary>0.3.1</summary>

将 SpineCodex 更新缓存与上游 Codex 安装隔离，避免产品更新与上游客户端相互影响。
</details>

<details>
<summary>0.3.0</summary>

将 Spine 迁移到采样边界运行时：Work Unit、递归 Spawn、Node Memory、replay 和 projection 由 SpineSDK 协调，并通过原生 Codex 体验呈现。运行时维护树状态和上下文投影，智能体聚焦当前单元。
</details>

<details>
<summary>0.2.2</summary>

首次公开 SpineJIT 设计：将线性消息流编译为 SpineTree，用 Node Memory 替换已完成分支，并支持递归扩展子智能体；Spine Spawn 和 Memory Projection 是最初的实验性界面。
</details>
</details>

## Spine 如何工作

LLM 消费线性上下文，而工作会递归展开。

- **智能体：** 处理当前 Work Unit。
- **Spine Runtime：** 将 Work Unit 持久化为 SpineBranch，并组合成 SpineTree。
- **SpineJIT：** 将当前分支投影为下一次模型采样所需的线性上下文。

```text
Work Unit -> SpineBranch -> SpineTree -> 当前分支上下文
```

智能体管理工作；Spine 在现有工作流背后维护递归状态。

## 长周期任务表现

在三项长周期编程基准测试中，SpineCodex 均取得了更好的结果：在 SWE-Milestone 上以低 27% 的总成本解决了 **1.89 倍的任务**，在 ProgramBench 上将平均得分提高 **10.80 个百分点**，并在 FrontierSWE 上将平均得分提高 **9.2 个百分点**。

<details>
<summary>基准测试详情</summary>

### SWE-Milestone（ICML 2026）

_长周期软件开发 · 80 个里程碑 · GPT-5.6 · sol high_

| 系统           | 已解决任务 |      总成本 |
| -------------- | ---------: | ----------: |
| BaseCodex      |          9 |     $764.18 |
| **SpineCodex** |     **17** | **$556.46** |

**以低 27% 的总成本解决了 1.89 倍的任务。**

### ProgramBench

_整仓程序重建 · 从 200 个任务中随机抽取 50 个 · GPT-5.6 · Sol high · 保守成本估算_

| 系统           |   平均得分 | 得分超过 95% 的任务数 |        成本 |
| -------------- | ---------: | --------------------: | ----------: |
| BaseCodex      |     62.55% |                  2/50 |     $188.12 |
| **SpineCodex** | **73.35%** |              **7/50** | **$475.10** |

**平均得分提高 10.80 个百分点，高分任务数达到 3.5 倍。**

### FrontierSWE

_超长周期编程 · 9 个任务评测 · GPT-5.6 · high · 每次试验的预估 API 成本_

| 系统           | 平均得分 | 最佳得分 |       成本 |
| -------------- | -------: | -------: | ---------: |
| BaseCodex      |     33.5 |     37.9 |     $20.16 |
| **SpineCodex** | **42.7** | **46.8** | **$37.29** |

**平均得分提高 9.2 个百分点，最佳得分提高 8.9 个百分点。**

</details>

## SpineJIT 如何工作

**智能体形态发生（Agent Morphogenesis）：** 每个任务都通过即时上下文树编译和递归子智能体扩展，塑造属于自己的上下文与执行过程。

**简而言之：** SpineJIT 用更短的记忆替换上下文中的活动后缀，同时保持前缀不变，使其可以继续命中提示词缓存。

为了精确控制这种后缀替换，SpineJIT 被实现为一条即时编译与上下文映射流水线：

$$
\text{上下文消息}
\rightarrow \text{Spine tokens}
\rightarrow \text{SpineTree (ParseStack)}
\rightarrow \text{新上下文}
$$

这条流水线包含两个主要阶段。

### 1. 将上下文即时编译为 SpineTree

SpineJIT 将上下文 $C$（消息列表，或更简单地说，一段由消息作为字符组成的句子）视为待编译的流。

在每个采样边界，它把新追加的消息和控制事件转换为 **Spine tokens**，并更新实时 LR(0) ParseStack：

SpineJIT 使用四种 token：

$$
\Sigma_{\mathrm{Spine}} = \{\mathrm{Message},\ \mathrm{Open},\ \mathrm{Close},\ \mathrm{SpineSpawnNode}\}
$$

`Message` 表示原始上下文项。`Open`、`Close` 和 `SpineSpawnNode` 是 SpineJIT 在对应采样边界发出的特殊 token。

$$
\begin{aligned}
\mathrm{SpineTree} &\to \mathrm{Nodes}\ \mathrm{End} \\
\mathrm{Nodes} &\to \mathrm{Node} \mid \mathrm{Nodes}\ \mathrm{Node} \\
\mathrm{Node} &\to \mathrm{Message} \mid \mathrm{SpineTreeNode} \\
\mathrm{SpineTreeNode} &\to \mathrm{Open}\ \mathrm{Nodes}\ \mathrm{Close} \mid \mathrm{SpineSpawnNode}
\end{aligned}
$$

`End` 只表示会话在逻辑上结束；进行中的会话永远不会发出它。因此，ParseStack 就是当前的 SpineTree，而归约 `Open Nodes Close -> SpineTreeNode` 会把一个已关闭子树转换为单个节点。

简而言之，SpineJIT 使用 LR(0) 即时编译将上下文 $C$ 映射为 SpineTree $PS$：

$$
PS = \mathrm{compile}(C)
$$

### 2. 将 SpineTree 映射为新上下文

现在可以将结构化 SpineTree 映射为更短的上下文，同时保留其稳定前缀。对于 ParseStack $PS$，定义：

$$
C' = f(PS) = \prod_{i=0}^{n} h(PS[i])
$$

$$
h(X) =
\begin{cases}
\prod_{x \in X} h(x), & X = \mathrm{Nodes} \\
\mathrm{raw}(X), & X = \mathrm{Message} \\
\mathrm{memory}(X), & X = \mathrm{SpineTreeNode} \\
\mathrm{spine\\_node\\_desc}(X), & X = \mathrm{Open}
\end{cases}
$$

这里的 $\prod$ 表示按顺序拼接。

映射规则刻意保持简单：

- `Message` 通过 $\mathrm{raw}(X)$ 保留原始内容。
- 已关闭的 `SpineTreeNode` 被其更短的 $\mathrm{memory}(X)$ 替代。
- 未匹配的 `Open` 用简洁的 $\mathrm{spine\\_node\\_desc}(X)$ 表示，帮助 LLM 划定当前活动 Spine 节点的边界。

随着解析推进，上下文后缀中已完成的工作被归约为 `SpineTreeNode`，再投影为记忆。更早的上下文保持不变：

$$
\mathrm{prefix} \cdot \mathrm{suffix}
\longrightarrow
\mathrm{prefix} \cdot \mathrm{memory}
$$

这就是 SpineJIT 的核心思想：在工作已经完成的位置压缩上下文，同时不让可复用前缀失效。

### 3. SpineJIT 如何插入 Spine 控制 token

LLM 根据当前上下文决定何时打开或关闭 `SpineTreeNode`。指导目标是尽可能提高剩余上下文与当前任务的平均相关性。

这里的**采样**是指一次完整的模型响应处理周期：包括响应本身以及响应产生的所有工具调用。

SpineJIT 暴露 Spine 工具，让 LLM 表达这些决策。在某个采样步骤中的工具调用成功后，SpineJIT 会在精确的边界插入相应控制 token：

| 工具调用      | 插入的 token          | 位置   |
| ------------- | --------------------- | ------ |
| `spine.open`  | `Open`                | 采样前 |
| `spine.close` | `Close`               | 采样前 |
| `spine.next`  | `Close Open`          | 采样前 |
| `spine.spawn` | 多个 `SpineSpawnNode` | 采样后 |

这些 token 将模型对任务边界的决策连接到 LR(0) 解析器。解析器持续更新 ParseStack，进而更新下一次采样所看到的上下文。

<p align="center">
  <a href="./.github/assets/spinecodex-loop-zh-cn.webp">
    <img src="./.github/assets/spinecodex-loop-zh-cn.webp" width="700" alt="SpineCodex 上下文树通过递归生成智能体不断生长" />
  </a>
  <br />
  <sub>点击查看完整动画。</sub>
</p>

## 引用

SpineJIT 技术报告即将发布。

如果你在研究中使用 SpineCodex，请引用本仓库：

```bibtex
@software{xiang2026spinecodex,
  title = {Agent Morphogenesis: Just-in-Time Context Tree Compilation for Cost-Efficient Recursive Subagent Scaling},
  author = {Jiahong Xiang and Kunqiu Chen and Yuqun Zhang},
  year = {2026},
  url = {https://github.com/GhabiX/SpineCodex}
}
```

## 项目

SpineCodex 是独立维护的 [OpenAI Codex CLI](https://github.com/openai/codex)（上游 0.153.4），由
[Jiahong Xiang](https://ghabix.github.io) 和
[Kunqiu Chen](https://camsyn.github.io) 维护。

SpineCodex 采用 [Apache-2.0 许可证](LICENSE)。OpenAI Codex 及其他派生组件的署名信息保留在 [NOTICE](NOTICE) 中。

## 参与贡献

欢迎提交 bug、参与 issue 讨论，也欢迎提出新功能想法。如果有新 feature 或 PR
想法，请先通过 issue 与我们沟通，确认方向、范围以及与当前 SpineSDK 和上游
Codex 版本的兼容性，再开始实现。任何 bug 都欢迎在
[GitHub Issues](https://github.com/GhabiX/SpineCodex/issues) 中反馈，我们会及时跟进并推动修复。也欢迎直接联系
[Jiahong Xiang](https://ghabix.github.io)。
