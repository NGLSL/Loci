# 14: 百万真实目录项达到查询、重启和资源目标

Task: LOCI-LINUX-001-14
Type: task
Parent: LOCI-LINUX-001
Publication: published — 用户已确认粒度和依赖
Status: claimed

**What to build:** 用户能对百万条真实库存持续搜索和更新，并得到明确的实际响应、启动和内存表现。

**Blocked by:** 12 — 短词与高匹配查询先返回首屏，完整计数另算；13 — 增量合并和事件风暴不影响可查询性，空闲开销低

**Acceptance criteria:**

- [ ] 至少 1,000,000 真实目录项、20,000 目录，规范名称/路径分布；按预算检查并创建专用夹具，合成字符串另报。
- [ ] 完整集合在建库、普通更新、rename、合并、重启和校正后与独立遍历对照，首屏/总数分开。
- [ ] 在规范参考硬件/文件系统上，30 类以上查询每类至少 200 次：首屏 p95 ≤50 ms、普通变化 p95 ≤500 ms、检查点可搜索 p95 ≤2 s。
- [ ] 稳态引擎进程 RSS ≤200 MiB、峰值 ≤512 MiB；内核对象开销与实际 watch/fd 单列，不能从进程内存推断已测内核成本。
- [ ] 报告建库、完整计数、全局排序、重命名子树、校正和目录密集最坏情况；不声称任何操作都在 50 ms 内。
- [ ] 资源或硬件不足时明确哪些门槛未验证，性能未达标时保持任务未完成并定位优化，不改变口径消失问题。

**Testing boundary:** 公开 Engine 与实际 CLI；真实 1M 全集合 oracle、计时和进程/内核资源采样。

**User stories covered:** 3, 10, 14, 23, 27, 31, 33

## Comments

- 2026-10-02：依据既有 Linux 规格，按 to-tickets 准备独立本地 ticket 草稿；确认后 Status 改为 ready-for-agent 并逐票发布。父规格和既有 01 任务未修改。


- 2026-10-02：用户批准“按现有拆分发布本地 issues”，随后授权按依赖开发并统一 review。

- 2026-10-02 21:53 UTC：12/13已resolved，按GO在独立worktree基于integration开始正式实现。先typedpage kinds避免EXPORT O(N²)、默认1.25M live headroom真实1M→1000001、独立worker实际RSS/首屏烟测，再完整benchmark/correctness/native准备。参考SSD/NVMe尚未确认，未验证门槛不会resolved。

- 2026-10-02 22:30 UTC：foundation b45dd8d已合入；actual1M独立Engine full rawbyte+kind oracle匹配1,000,000entries/20,000dirs。52query×200×2，rr第二轮p95=51.467865ms超50ms、第一轮49.855ms；ia47.248/48.049ms、ii11.815/12.461ms，其余query-pass numeric达标。RSS稳态约168.81MiB，checkpoint峰值约215.18MiB。失败原始样本/workspace/linux-ticket-14-queries-b45dd8/results保留。该开发基线还无filtered语义独立校验/参考SSD证明，不作为完整验收。主implementer继续dense/subtree/校正；12 owner独立verifier、13 owner短词performancefix并行准备，保持samecorpus/suite/metric；14未resolved。

- 2026-10-02 23:00 UTC：Pair256 core/docs16a617d已整合，同52×200×2源86d2941全部104query-pass p95通过，最差14.837ms、rr12.318ms、查询稳态184.02MiB/保存峰230.45。目录dense校正63ea584由旧67–72s降至1.18–1.26s且完整集正确。独立verifier抓出Greek-invalid-path ext漏项，871b523修复小native52suite/原失败fixture绿正在轻量合入。实际1M source16a617d stages完整集、旧lease、合并、rename/恢复、3校正/stop均正确，但释放全部lease/jobs后稳态RSS324/318/335/372MiB，单Data约116MB、peak421MiB，200目标失败；13owner在独立memory-reclamation worktree定位并修复，原始失败曲线/workspace/linux-ticket-14-stages-16a617d/results保留。14仍claimed未完成；未开始正式whole review/最终native同SHA验收。

- 2026-10-02 23:36 UTC：actual1M maintenance RSS修复复测：trim-only cce7ec1 postcompact194.23MiB但3correct223.94/222.57/228.15MiB；私有mapped entry/name/filter源459153d完整集合、lease、stop正确，最后correction steady208.24MiB仍超过200，继续定位。各阶段原始timings/resources保留，见evidence/final-acceptance.json development_evidence；14仍claimed，统一review/final native/24h/Windows尚未完成。Btrfs100k capacity helper及guest source glue正在归档，不作为native结果。

- 2026-10-03 00:05 UTC：f4ff0cd actual1M相同fixture维护复测：postcompact185.41、correct0 221.49超200、correct1 186.21、correct2 186.05MiB；完整byte/kind、旧lease、cancel、stop正确，仍有epoch超标故继续修复且14claimed。root已合2e704dc Btrfs环境源码与ff543 Btrfs100k预检，source archival不是native验收；ext4环境/只读assessment工具归档中，最终统一review/同SHA原生/真实24h尚待。

- 2026-10-03 00:14 UTC：c8e035f packed directory prefixes actual1M maintenance cuts192.52/186.97/210.18/187.00MiB，其中correct1仍失败。完整结果和释放行为正确，保留/workspace/linux-million-memory-prefix-c8e035f/results。接下来development-only手工trim诊断只辨别释放时机/碎片，不作为production GREEN。root已a0a70e ext4环境与ebca55c只读assessment全部归档，等待内存最终修复及全改review。

- 2026-10-03：ed2ec025全改审查完成：Standards3（2硬性P2、1判断P3）；Spec6（2实现P2、4验收缺口P1）。unified_review_fixes单一implementer处理depth/path rename、可信mkdir局部有界scan、public seam测试和typed错误分类；外部gates继续未完成。报告reviews/*-ed2ec025.md。a776四epoch数值green只开发proof。已用相同SHA原始闭合artifact在各run内部hardlink去重48files释放约6.26GB，所有raw路径和内容保持，原inode/mtime维护receipt /workspace/loci-development-evidence-dedup-20261003.jsonl。最终ext4环境image sha256:9e8fb46877bb81fc360890d6a158fc6330a5c43b187b7af8a49ac260ece5d04e，sourceglue/65dependencies pinned，UID1000 realext4 write及strict挂载/loop退出释放smoke成功，日志/workspace/loci-ext4-env-smoke-ed2ec025.log；仅环境能力非final Engine门槛。

- 2026-10-03：当前容量复用候选0c2df04b已整合并完成e204起69提交统一双轴review（Standards0/Spec5P1含尚未最终GREEN的RSS问题）。原516真实ext4204.375MiB与独立causal206.977MiB RED全部保留。新exact0c2 ext4完整百万流程正在运行，52query×200×2 worstp95 12.482441ms，三类200event p95 32.390050/38.958684/48.166340ms；四次无lease单epoch维护RSS177.0586/177.6406/177.6055/177.6094MiB均数值通过。仍需原600sidle、200跨进程重开/资源释放以及剩余同版本Btrfs/目录密集/参考硬件验证，不提前resolved。raw `/workspace/loci-native-final-ext4-0c2df04b/million/`。
