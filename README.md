# Task Forge

A flexible and asynchronous task forge for concurrent task execution in Rust. `Task Forge` allows you to spawn tasks, send messages, and handle task outputs efficiently using Tokio channels.

---

## **Table of Contents**
- [Features](#features)
- [Installation](#installation)
- [Usage](#usage)
  - [Basic Example](#basic-example)
  - [Waiting for Task Creation](#waiting-for-task-creation)
  - [Handling Concurrent Tasks](#handling-concurrent-tasks)
- [Error Handling](#error-handling)
- [Documentation](#documentation)
- [License](#license)

---

## **Features**
- Spawn and manage multiple asynchronous tasks.
- Send messages to tasks and receive their outputs.
- Track task states (`Running`, `Closed`).
- Automatically notify when tasks complete or when the forge is cleaned.
- Flexible task creation with support for generic task arguments.
- Customizable error handling using different error handlers.
- Wait for all tasks to end with an optional timeout.

---

## **Installation**
To add `task_forge` to your project, include the following in your `Cargo.toml`:
```toml
[dependencies]
task_forge = "0.1.1"  # Replace with the latest version
tokio = { version = "1", features = ["full"] }
```

---

## **Examples**

# Basic Example

Below is a simple example using the `TaskForge` to create and run a basic task:

```rust
use task_forge::{task::TaskTrait, Sender, Receiver, channel, TaskForge, task::TaskInterface};

struct EchoTask;

impl TaskTrait<String, String, String> for EchoTask {
    fn begin(
        _: String,
        mut message_receiver: Receiver<String>,
        task_interface: TaskInterface<Output>,
    ) {
        tokio::spawn(async move {
            if let Some(input) = message_receiver.recv().await {
                task_interface
                    .output(format!("Echo: {input}"))
                    .await
                    .unwrap();
            }
        });
    }
}

#[tokio::main]
async fn main() {
    let (task_forge, _) = TaskForge::<String, String>::new();

    let task_id = 1;
    task_forge.new_task::<EchoTask, _>(task_id, "Hello".to_string()).await.unwrap();
    task_forge.send(task_id, "Hello again!".to_string()).await.unwrap();

    let mut result_receiver = task_forge.new_result_redirection().await;
    let result = result_receiver.recv().await.unwrap();
    assert_eq!(result.output.as_ref(), "Echo: Hello again!");
}
```

# Waiting for Task Creation

You can ensure that a task is fully created before sending a message using `wait_for_task_creation`:
```rust
let task_id = 42;
tokio::spawn(async move {
    task_forge.wait_for_task_creation(task_id).await.unwrap();
    task_forge.send(task_id, "Message after creation".to_string()).await.unwrap();
});
```
Or you can use `wait_and_send` which does the same work:
```rust
let task_id = 42;
tokio::spawn(async move {
    task_forge.wait_and_send(task_id, "Message after creation".to_string()).await.unwrap();
});
```

# Handling Concurrent Tasks

You can spawn and manage multiple tasks concurrently:
```rust
struct IncrementTask;
impl TaskTrait<u64, u64, u64> for IncrementTask {
    fn begin(
        init_val: u64,
        mut message_receiver: Receiver<u64>,
        task_interface: TaskInterface<u64>,
    ) {
        tokio::spawn(async move {
            if let Some(val) = message_receiver.recv().await {
                task_interface.output(init_val + val).await.unwrap();
            }
        });
    }
}


for i in 0..5 {
    task_forge.new_task::<IncrementTask, _>(i, i).await.unwrap();
    task_forge.send(i, 10).await.unwrap();
}
```

---

## **Error Handling**

`task_forge` provides several built-in error handling mechanisms:
- `panic_error_handler`: Panics when an error occurs
- `log_error_handler`: Logs the errors
- `ignore_error_handler`: Silently ignores all errors
    
To use a custom error handler, pass the appropriate receiver when creating the forge:
```rust
use task_forge::log_error_handler;

let (task_forge, error_receiver) = TaskForge::new();
log_error_handler(error_receiver);
```
You can also create your own error handler by using the error receiver.

## **Licence**

This project is licensed under the MIT Licence. Let me know if you’d like any changes or additions!
