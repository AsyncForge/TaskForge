use crate::{
    errors::{ForgeError, ForgeResult},
    task_forge::OpId,
    Receiver, Sender,
};

type TaskOutputSender<Output> = Sender<(OpId, Output)>;

/// This struct is given to each task at the begining of it
pub struct TaskInterface<Output> {
    pub(crate) id: OpId,
    pub(crate) output_sender: TaskOutputSender<Output>,
}

impl<Output> TaskInterface<Output> {
    /// Returns the id of the task
    pub fn id(&self) -> OpId {
        self.id
    }

    /// Send the output to the pool via a channel, returns an error if the sender fail to handle the message
    pub async fn output(self, output: Output) -> ForgeResult<OpId> {
        if self.output_sender.send((self.id, output)).await.is_err() {
            Err(ForgeError::FailedToOutput(self.id))
        } else {
            Ok(self.id)
        }
    }
}

/// To use a task forge you should implement this trait to your tasks.
pub trait TaskTrait<Arg, Message, Output> {
    /// The begin method takes in parameter an arg of any type that you can pass to your task, a receiver able to receive some message from the pool, and the task interface
    fn begin(arg: Arg, message_receiver: Receiver<Message>, interface: TaskInterface<Output>);
}
