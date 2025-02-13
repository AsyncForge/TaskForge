use crate::{
    channel,
    errors::ErrorReceiver,
    errors::{panic_error_handler, ForgeError, ForgeResult},
    task::{TaskInterface, TaskTrait},
    Receiver, Sender,
};
use std::marker::Send;
use std::{collections::HashMap, sync::Arc};
use tokio::{select, sync::RwLock, time::Duration};
use tracing::error;

pub(crate) const CHANNEL_SIZE: usize = 100;

type Wrapped<T> = Arc<RwLock<T>>;
type Result<T> = std::result::Result<T, ForgeError>;

pub type OpId = u64;

#[derive(PartialEq, Eq, Copy, Clone)]
enum TaskState {
    Running,
    Closed,
}

/// This struct represents the result of a completed task. It contains the output, the task ID, and a boolean indicating whether the output cleared the forge (true) or not (false).
#[derive(Debug, Clone)]
pub struct ForgeTaskEnded<Output: Send> {
    pub output: Arc<Output>,
    pub id: OpId,
    pub has_cleared: bool,
}

pub type OutputSender<Output> = Sender<ForgeTaskEnded<Output>>;
pub type OutputReceiver<Output> = Receiver<ForgeTaskEnded<Output>>;

struct WrappedTaskForge<Message: Send, Output: Send> {
    forge: HashMap<OpId, Sender<Message>>,
    end_task_sender: Sender<(OpId, Output)>,
    output_result_senders: Vec<OutputSender<Output>>,
    task_creation_senders: HashMap<OpId, Vec<Sender<()>>>,
    task_states: HashMap<OpId, TaskState>,
    cleaning_notifier: Option<(Sender<Result<()>>, Option<usize>)>,
}

impl<Message: Send + 'static, Output: Send + 'static + Sync> WrappedTaskForge<Message, Output> {
    fn new(error_sender: Sender<ForgeError>) -> Wrapped<Self> {
        let (end_task_sender, receiver) = channel(CHANNEL_SIZE);
        let forge = Arc::new(RwLock::new(WrappedTaskForge {
            forge: HashMap::new(),
            end_task_sender,
            output_result_senders: Vec::new(),
            task_creation_senders: HashMap::new(),
            task_states: HashMap::new(),
            cleaning_notifier: None,
        }));
        Self::listen_for_ending_task(forge.clone(), receiver, error_sender);
        forge
    }

    fn should_clear(&self) -> bool {
        if let Some((_, awaited)) = self.cleaning_notifier {
            let awaited_satisfied = if let Some(awaited) = awaited {
                self.task_states.len() == awaited
            } else {
                true
            };
            self.cleaning_notifier.is_some() && self.forge.is_empty() && awaited_satisfied
        } else {
            false
        }
    }

    /// Removes a task from the forge based on its ID. If the forge is in the cleaning stage, the task is fully removed; otherwise, its state is set to "Closed".
    /// This function may fail if the task has already ended or if the cleaning notifier is uninitialized or closed.
    async fn handle_ending_task(&mut self, ending_task: OpId) -> ForgeResult<bool> {
        if self.task_states.get(&ending_task) != Some(&TaskState::Running) {
            return Err(ForgeError::TaskClosed(ending_task));
        }
        self.forge.remove(&ending_task);

        self.task_states.insert(ending_task, TaskState::Closed);
        let should_clear = self.should_clear();
        if should_clear {
            self.task_states.clear();
            self.task_creation_senders.clear();
            let notifier = self.cleaning_notifier.take().unwrap().0; // Can't fail
            if notifier.send(Ok(())).await.is_err() {
                return Err(ForgeError::CleaningNotifierError);
            }
        }
        Ok(should_clear)
    }

    /// This function represents the main loop of the forge where all completed tasks are handled.
    /// If handling a task fails, the error is sent through the `error_sender`. If `error_sender`
    /// is not valid, the function log the errors.
    fn listen_for_ending_task(
        forge: Wrapped<Self>,
        mut receiver: Receiver<(OpId, Output)>,
        error_sender: Sender<ForgeError>,
    ) {
        tokio::spawn(async move {
            loop {
                let (ending_task, result) = match receiver.recv().await {
                    Some(r) => r,
                    None => {
                        return;
                    }
                };
                let mut forge = forge.write().await;
                match forge.handle_ending_task(ending_task).await {
                    Ok(has_cleared) => {
                        let mut senders_to_remove = Vec::new();
                        let result = Arc::new(result);
                        for (i, sender) in forge.output_result_senders.iter().enumerate() {
                            let output = ForgeTaskEnded::<Output> {
                                id: ending_task,
                                output: Arc::clone(&result),
                                has_cleared,
                            };
                            if sender.send(output).await.is_err() {
                                senders_to_remove.push(i)
                            }
                        }

                        for to_remove in senders_to_remove.into_iter().rev() {
                            forge.output_result_senders.remove(to_remove);
                        }
                    }
                    Err(e) => {
                        if let Err(e) = error_sender.send(e).await {
                            error!("Failed to send error: {e:?}");
                        }
                    }
                }
            }
        });
    }

    async fn new_task<Task: TaskTrait<TaskArg, Message, Output>, TaskArg: Send + 'static>(
        &mut self,
        id: OpId,
        arg: TaskArg,
    ) -> ForgeResult<()> {
        if self.task_states.insert(id, TaskState::Running) == Some(TaskState::Running) {
            return Err(ForgeError::TaskAlreadyExists(id));
        }
        let mut result = Ok(());
        if let Some(senders) = self.task_creation_senders.remove(&id) {
            for s in senders {
                if s.send(()).await.is_err() {
                    result = Err(ForgeError::TaskCreationSendingError(id));
                };
            }
        }
        let (message_sender, message_receiver) = channel(CHANNEL_SIZE);
        let output_sender = self.end_task_sender.clone();
        tokio::spawn(async move {
            Task::begin(arg, message_receiver, TaskInterface { id, output_sender });
        });
        self.forge.insert(id, message_sender);
        result
    }

    async fn wait_for_task_creation(forge: &Wrapped<Self>, id: OpId) -> ForgeResult<()> {
        let mut receiver = {
            let mut forge = forge.write().await;
            let state = forge.task_states.get(&id);
            match state {
                None => forge.new_task_notif_receiver(id),
                Some(TaskState::Running) => return Ok(()),
                Some(TaskState::Closed) => return Err(ForgeError::TaskClosed(id)),
            }
        };
        if receiver.recv().await.is_none() {
            return Err(ForgeError::FailedToWaitCreation(id));
        }
        Ok(())
    }

    async fn wait_and_send(forge: &Wrapped<Self>, id: OpId, msg: Message) -> ForgeResult<()> {
        Self::wait_for_task_creation(forge, id).await?;
        forge.read().await.send(id, msg).await
    }

    async fn send(&self, id: OpId, msg: Message) -> ForgeResult<()> {
        match self.forge.get(&id) {
            Some(sender) => {
                if sender.send(msg).await.is_err() {
                    return Err(ForgeError::SendError(id));
                };
                Ok(())
            }
            None => Err(ForgeError::TaskNotExist(id)),
        }
    }

    fn new_result_redirection(&mut self) -> OutputReceiver<Output> {
        let (sender, receiver) = channel(CHANNEL_SIZE);
        self.output_result_senders.push(sender);
        receiver
    }

    fn new_task_notif_receiver(&mut self, id: OpId) -> Receiver<()> {
        let (sender, receiver) = channel(CHANNEL_SIZE);
        match self.task_creation_senders.get_mut(&id) {
            Some(senders) => senders.push(sender),
            None => {
                let _ = self.task_creation_senders.insert(id, vec![sender]);
            }
        }
        receiver
    }

    fn is_empty(&self) -> bool {
        self.forge.is_empty()
    }

    fn clean(
        &mut self,
        wrapped_self: Wrapped<Self>,
        awaited: Option<usize>,
        timer: Option<Duration>,
    ) -> Option<Receiver<Result<()>>> {
        let (sender, receiver) = channel::<Result<()>>(100);
        if self.forge.is_empty() {
            self.task_creation_senders.clear();
            self.task_states.clear();
            None
        } else {
            match timer {
                Some(dur) => {
                    let (timer_sender, mut timer_receiver) = channel(CHANNEL_SIZE);
                    self.cleaning_notifier = Some((timer_sender, awaited));
                    tokio::spawn(async move {
                        let sleep = tokio::time::sleep(dur);
                        tokio::pin!(sleep);
                        let timer = timer_receiver.recv();
                        tokio::pin!(timer);
                        select!(
                            res = &mut timer => {
                                sender.send(res.unwrap()).await.unwrap();
                            },
                            _ = &mut sleep => {
                                let remain = {
                                    let mut forge = wrapped_self.write().await;
                                    let remain = forge.forge.len();
                                    forge.forge.clear();
                                    forge.task_states.clear();
                                    forge.task_creation_senders.clear();
                                    forge.cleaning_notifier = None;
                                    remain
                                };
                                sender.send(Err(ForgeError::CleaningTimeOut(remain))).await.unwrap();
                            }
                        );
                    });
                }
                None => {
                    self.cleaning_notifier = Some((sender, awaited));
                }
            }
            Some(receiver)
        }
    }
}

/// This is an implementation of a generic task forge . The forge can instantiate task
/// and send messages to the spawned tasks. It also tracks the state of each task
/// and provides a sender for easily communicating messages through a Tokio channel.
///
/// As a generic task forge, it defines a `Task` trait, which includes a `begin` method
/// that returns a sender for message transmission. Messages are generic, but all tasks
/// within the forge must handle the same message type, even if the tasks themselves differ.
///
/// The forge is also able to wait for the creation of a specifiv task, the g
///
/// Additionally, the forge can externally notify about task creation and termination events.
/// When a task generates output, it sends the result back to the forge through a sender,
/// and the forge broadcasts this result to all interested parties.
pub struct TaskForge<Message: Send, Output: Send + Sync> {
    forge: Wrapped<WrappedTaskForge<Message, Output>>,
}

impl<Message: Send + 'static, Output: Send + 'static + Sync> Default
    for TaskForge<Message, Output>
{
    fn default() -> Self {
        let (sender, receiver) = channel(CHANNEL_SIZE);
        panic_error_handler(receiver);
        Self {
            forge: WrappedTaskForge::new(sender),
        }
    }
}

impl<Message: Send + 'static, Output: Send + 'static + Sync> Clone for TaskForge<Message, Output> {
    fn clone(&self) -> Self {
        Self {
            forge: self.forge.clone(),
        }
    }
}

impl<Message: Send + 'static, Output: Send + 'static + Sync> TaskForge<Message, Output> {
    /// This function creates a forge and return it along with a receiver to handle errors in result reception.
    /// Note that the forge implements Default, the default implemenation uses panic_error_handler to handle errors
    pub fn new() -> (Self, ErrorReceiver) {
        let (sender, receiver) = channel(CHANNEL_SIZE);
        (
            Self {
                forge: WrappedTaskForge::new(sender),
            },
            receiver,
        )
    }

    /// Creates a new task and adds it to the forge. The task must implement `TaskTrait`, allowing the forge to start it with a generic argument via the `begin` function.
    /// This function may fail if the task is already running, but not if the task was previously running and is now closed.
    /// If a `task_creation_waiter` is waiting for the task ID, a notification is sent.
    /// Returns an error if one of the senders is invalid.
    pub async fn new_task<Task: TaskTrait<TaskArg, Message, Output>, TaskArg: Send + 'static>(
        &self,
        id: OpId,
        config: TaskArg,
    ) -> ForgeResult<()> {
        self.forge
            .write()
            .await
            .new_task::<Task, TaskArg>(id, config)
            .await
    }

    /// This function waits the creation of a task and send it the generic message passed in argument. The function fail if the task is closed or if we fail to receiv the creation notification
    pub async fn wait_for_task_creation(&self, id: OpId) -> ForgeResult<()> {
        WrappedTaskForge::wait_for_task_creation(&self.forge, id).await
    }

    ///  Returns a receiver of task result, when a task ended the result will be sent through it
    pub async fn new_result_redirection(&self) -> OutputReceiver<Output> {
        self.forge.write().await.new_result_redirection()
    }

    /// Returns true if there is no running process in the forge
    pub async fn is_empty(&self) -> bool {
        self.forge.read().await.is_empty()
    }

    /// Waits for a task to be created and sends a message to it.
    /// Fails if the wait fails or if inter-thread communication encounters an error.
    pub async fn wait_and_send(&self, id: OpId, msg: Message) -> ForgeResult<()> {
        WrappedTaskForge::wait_and_send(&self.forge, id, msg).await
    }

    /// This function takes a message and an id and try to send via the task sender the message to the running task. The function may return an error if the task does not exist or is over, or if the sender fail to give the message.
    pub async fn send(&self, id: OpId, msg: Message) -> ForgeResult<()> {
        self.forge.read().await.send(id, msg).await
    }

    /// This function put the forge in cleaning phase.
    /// The awaited parameter represents the number of process that have to be awaited, if None we simply wait for the forge to be empty.
    /// The timer parameter represents the time that we have to wait before throwing an error through the output sender. If None, the pool will wait infinitely.
    /// If the forge is already cleaned, then the function returns None directly, otherwise, the function bring the forge in cleaning phase and returns a receiver that will notify when the forge is cleaned
    pub async fn clean(
        &self,
        awaited: Option<usize>,
        timer: Option<Duration>,
    ) -> Option<Receiver<Result<()>>> {
        let w = self.forge.clone();
        self.forge.write().await.clean(w, awaited, timer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type Message = String;
    type Output = String;
    type Arg = String;

    #[tokio::test]
    async fn test_simple_task_execution() {
        let (task_forge, _) = TaskForge::<Message, Output>::new();

        struct EchoTask;

        impl TaskTrait<Arg, Message, Output> for EchoTask {
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

        let task_id = 1;
        task_forge
            .new_task::<EchoTask, _>(task_id, "Hello".to_string())
            .await
            .unwrap();
        task_forge
            .send(task_id, "Hello again!".to_string())
            .await
            .unwrap();

        let mut result_receiver = task_forge.new_result_redirection().await;
        let result = result_receiver.recv().await.unwrap();
        assert_eq!(result.output.as_ref(), "Echo: Hello again!");
    }

    #[tokio::test]
    async fn test_task_does_not_exist() {
        let (task_forge, _) = TaskForge::<String, String>::new();
        let task_id = 999;
        let result = task_forge.send(task_id, "Invalid task".to_string()).await;

        assert!(matches!(result, Err(ForgeError::TaskNotExist(_))));
    }

    #[tokio::test]
    async fn test_forge_cleaning() {
        let (task_forge, _) = TaskForge::<String, String>::new();

        struct DummyTask;
        impl TaskTrait<(), String, String> for DummyTask {
            fn begin(_: (), _: Receiver<Message>, task_interface: TaskInterface<Output>) {
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    let _ = task_interface.output("Completed".to_string()).await;
                });
            }
        }

        let none_cleaner = task_forge.clean(None, None).await;
        assert!(none_cleaner.is_none());

        let awaited = 200;
        for i in 0..awaited {
            task_forge.new_task::<DummyTask, _>(i, ()).await.unwrap();
        }

        let cleaner = task_forge.clean(Some(awaited as usize), None).await;
        assert!(cleaner.is_some());
        cleaner.unwrap().recv().await;

        let none_cleaner = task_forge.clean(None, None).await;
        assert!(none_cleaner.is_none());
    }

    #[tokio::test]
    async fn test_forge_cleaning_timeout() {
        let (task_forge, _) = TaskForge::<String, String>::new();

        struct InfTask;
        impl TaskTrait<(), String, String> for InfTask {
            fn begin(_: (), _: Receiver<Message>, _: TaskInterface<String>) {}
        }

        task_forge.new_task::<InfTask, _>(0, ()).await.unwrap();

        let cleaner = task_forge
            .clean(None, Some(Duration::from_millis(200)))
            .await;
        assert!(cleaner.is_some());
        assert!(matches!(
            cleaner.unwrap().recv().await,
            Some(Err(ForgeError::CleaningTimeOut(1)))
        ))
    }

    #[tokio::test]
    async fn test_send_concurrent_tasks() {
        let (task_forge, _) = TaskForge::<u64, u64>::new();

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

        let mut result_receiver = task_forge.new_result_redirection().await;
        for _ in 0..5 {
            let result = result_receiver.recv().await.unwrap();
            assert_eq!(result.output.as_ref(), &(result.id + 10));
        }
    }

    #[tokio::test]
    async fn test_wait_and_send_concurrent_tasks() {
        let (task_forge, _) = TaskForge::<u64, u64>::new();

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
            let task_forge = task_forge.clone();
            tokio::spawn(async move { task_forge.wait_and_send(i, 10).await.unwrap() });
        }

        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

        for i in 0..5 {
            task_forge.new_task::<IncrementTask, _>(i, i).await.unwrap();
        }

        let mut result_receiver = task_forge.new_result_redirection().await;

        for _ in 0..5 {
            let result = result_receiver.recv().await.unwrap();
            assert_eq!(result.output.as_ref(), &(result.id + 10));
        }
    }
}
