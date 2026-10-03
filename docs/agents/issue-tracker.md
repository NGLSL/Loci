# Issue tracker: Local Markdown

本项目的规格、任务和决策记录保存在本地 `.scratch/`。技能中的“发布到任务跟踪系统”指写入本地 Markdown 文件。

## 规格与实施任务

- 每项功能使用独立目录：`.scratch/<feature-slug>/`。
- 规格：`.scratch/<feature-slug>/spec.md`。
- 实施任务：`.scratch/<feature-slug>/issues/<NN>-<slug>.md`，每个任务一个文件，从 `01` 开始按依赖顺序编号。
- 文件顶部用 `Status:` 记录状态，用 `Category:` 记录 `bug` 或 `enhancement`；映射见 `triage-labels.md`。
- 用 `Blocked by:` 列出前置任务编号；没有依赖时写 `None`。
- 每个实施任务列出可验证的验收条件。完成后标记验收项，并将 `Status:` 设为 `done`；`done` 是完成状态，区别于 triage 的五种分流状态。
- 评论和讨论追加到文件底部的 `## Comments`。
- 读取任务时打开用户指定的文件；仅提供编号时，在对应功能目录内查找。编号跨功能不唯一，存在歧义时确认目录。

按需创建目录和文件；初始化只配置规则，不创建示例任务或空白规格。

## Wayfinding operations

`wayfinder` 的决策图与实施任务使用相同的本地目录约定。

- 决策图：`.scratch/<effort>/map.md`，包含目标、约定、已决策索引、尚未明确的问题和范围之外的事项。
- 子决策任务：`.scratch/<effort>/issues/<NN>-<slug>.md`，正文说明需要解决的问题。
- `Type:` 为 `research`、`prototype`、`grilling` 或 `task`。
- `Status:` 初始为 `open`，领取后为 `claimed`，解决后为 `resolved`。这些状态用于决策任务，区别于实施任务的 triage 状态。
- `Blocked by:` 列出依赖的任务编号；只有全部依赖均为 `resolved` 时才能开始。
- 待处理任务是状态为 `open` 且无未解决依赖的任务，按编号选择。
- 领取时先写入 `Status: claimed` 并保存，再开始工作。
- 解决时在 `## Answer` 下追加结论，设置 `Status: resolved`，再向 `map.md` 的已决策索引追加简述和任务链接。

此配置采用本地文件，不调用 GitHub、GitLab 或其他外部任务系统。

## 版本管理与临时工件

- `.scratch/<feature-slug>/spec.md`、`issues/*.md`、`map.md` 是可评审的项目记录，应提交；历史状态按原记录保留，不能将保留记录解释为已重新验收。
- `docs/adr/*.md`、`GLOSSARY.md` 与 `docs/agents/*.md` 同样版本管理。共享接口提案使用明确的 proposed 状态，接受或替换时记录决定。
- 工作树、构建缓存、原生日志、测试文件夹与临时讨论留在明确的生成目录中。给新工件目录添加具体忽略项，保留其同级规格和任务的可见性。
- 本项目不采用忽略整个 `.scratch/` 或 `docs/agents/` 的规则。新增记录后用 `git status --short --untracked-files=all` 检查可见性，用 `git check-ignore -v <path>` 定位命中的工件规则；未忽略路径返回退出码 1 是预期结果。
