# Long calls and tasks

A tool call can run for minutes or hours, as long as it shows signs of
life:

```toml
[gateway]
call_timeout = "2m"     # the most a call may go without its result or a progress notification
max_call_time = "1h"    # the most it may run at all
```

- **Progress.** A backend's `notifications/progress` reach the client
  under the client's own `progressToken`. Each one restarts the
  `call_timeout` clock.
- **Cancellation.** A client that cancels a request, or goes away, stops the
  tool at the backend: mcpgw sends the backend `notifications/cancelled`.
- **Tasks** (the `io.modelcontextprotocol/tasks` extension, SEP-2663). For
  a client that declares the extension, a call still running after 1
  second becomes a task.
  - The client gets the task's id at once and polls `tasks/get`.
  - The task's `statusMessage` follows the call's progress.
  - `tasks/cancel` stops the tool at the backend.
  - Tasks are kept for an hour, and only the endpoint that created a task
    can see it.
- **Backends that run tasks themselves** are followed to the end, and their
  status messages become progress. Clients see the same thing either way.

Verified at full length: a 10-minute call delivers 1,200 progress
notifications, and the same call cancelled near the end stops at the
backend (`crates/mcpgw/tests/long_calls.rs`, `ten_minute_call`).
