//! # Task Forge
//!
//! **Task Forge** is a simple and flexible library for managing asynchronous task execution in Rust.  
//! It provides an efficient `TaskForge` structure for spawning, tracking, and communicating with tasks concurrently.
//!
//! ## Features
//! - Spawn and manage multiple asynchronous tasks.
//! - Send messages to tasks and receive their outputs.
//! - Track task states (`Running`, `Closed`).
//! - Automatically notify when tasks complete or when the forge is cleaned.
//! - Flexible task creation with support for generic task arguments.
//! - Customizable error handling using different error handlers.
//! - Wait for all tasks to finish with an optional timeout.
//!
//! ## Example Usage
//! ```rust
//! use task_forge::{task::TaskTrait, Sender, Receiver, channel, TaskForge, task::TaskInterface};
//!
//! struct EchoTask;
//!
//! impl TaskTrait<String, String, String> for EchoTask {
//!     fn begin(
//!         _: String,
//!         mut message_receiver: Receiver<String>,
//!         task_interface: TaskInterface<String>,
//!     ) {
//!         tokio::spawn(async move {
//!             if let Some(input) = message_receiver.recv().await {
//!                 task_interface
//!                     .output(format!("Echo: {input}"))
//!                     .await
//!                     .unwrap();
//!             }
//!         });
//!     }
//! }
//!
//! #[tokio::main]
//! async fn main() {
//!     let (task_forge, _) = TaskForge::<String, String>::new();
//!
//!     let task_id = 1;
//!     task_forge.new_task::<EchoTask, _>(task_id, "Hello".to_string()).await.unwrap();
//!     task_forge.send(task_id, "Hello again!".to_string()).await.unwrap();
//!
//!     let mut result_receiver = task_forge.new_result_redirection().await;
//!     let result = result_receiver.recv().await.unwrap();
//!     assert_eq!(result.output.as_ref(), "Echo: Hello again!");
//! }
//! ```

/// This module contains error handling types.
pub mod errors;
/// Contains the trait definitions for tasks.
pub mod task;
/// The implementation of the task forge.
pub mod task_forge;

pub use task::TaskTrait;
pub use task_forge::{OpId, TaskForge};
pub use tokio::sync::mpsc::{channel, Receiver, Sender};

/// Type alias for a communication channel.
pub type Channel<T> = (Sender<T>, Receiver<T>);

/// Creates a new message channel with a fixed buffer size.
pub fn new_channel<T: Send>() -> Channel<T> {
    tokio::sync::mpsc::channel(task_forge::CHANNEL_SIZE) // Taille configurable
}
