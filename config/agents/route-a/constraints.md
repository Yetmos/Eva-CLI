# Route A Agent Constraints

## 中文

- 只处理 `/sys/route-a` 范围内的路由任务。
- 通过 Scheduler 和 Topic route 下发事件，不直接调用子 Agent。
- 不访问 Adapter provider 或外部 I/O。

## English

- Handle routing work only under `/sys/route-a`.
- Dispatch through Scheduler and Topic routes instead of calling child Agents directly.
- Do not access Adapter providers or external I/O.
