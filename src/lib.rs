//! # Task Forge
//! A simple and flexible task forge for asynchronous task execution in Rust.
//! This library provides a `TaskForge` to manage concurrent tasks efficiently.

/// This module contains error handling types.
pub mod errors;
/// Contains the trait definitions for tasks.
pub mod task;
/// The implementation of the task forge.
pub mod task_forge;

pub use task::TaskTrait;
pub use task_forge::{OpId, TaskForge};
pub use tokio::sync::mpsc::{channel, Receiver, Sender};

pub type Channel<T> = (Sender<T>, Receiver<T>);
pub fn new_channel<T: Send>() -> Channel<T> {
    tokio::sync::mpsc::channel(100) // Taille configurable
}
