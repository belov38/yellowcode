pub mod storage;

use crate::{Limits, MemoryBudget, discovery::Visibility, lock::Lock};
use std::net::TcpStream;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Target {
    pub budget: u64,
    pub limits: Limits,
    pub row_limit: u64,
    pub response_limit: u64,
    pub visibility: Visibility,
    pub pressure: bool,
}
impl Target {
    fn conservative(self, other: Self) -> Self {
        let mut categories = self.limits.categories;
        for (cap, other) in categories.iter_mut().zip(other.limits.categories) {
            *cap = (*cap).min(other);
        }
        Self {
            budget: self.budget.min(other.budget),
            limits: Limits {
                admission: self.limits.admission.min(other.limits.admission),
                categories,
            },
            row_limit: self.row_limit.min(other.row_limit),
            response_limit: self.response_limit.min(other.response_limit),
            visibility: if self.visibility == other.visibility {
                self.visibility
            } else {
                Visibility::Partial
            },
            pressure: self.pressure || other.pressure,
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Phase {
    StartupClosed,
    Enabled,
    Updating,
    Pressure,
    Stopping,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkerState {
    Starting,
    Idle,
    Busy,
    Stopped,
    StartupFailed,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Reject {
    Busy,
    Pressure,
    Stopping,
}
#[derive(Debug)]
pub struct Rejected {
    pub reason: Reject,
    pub socket: TcpStream,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlError {
    InvalidInitialState,
    InvalidTransition,
    Stopped,
    GenerationExhausted,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Publication {
    Installed,
    Stopped,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct View {
    pub target: Target,
    pub generation: u64,
    pub phase: Phase,
    pub worker: WorkerState,
}
struct State {
    last_published: Target,
    visible_floor: Target,
    pending: Option<Target>,
    generation: u64,
    phase: Phase,
    worker: WorkerState,
    claimed_generation: u64,
    socket: Option<TcpStream>,
}
pub struct Control<'m> {
    memory: &'m MemoryBudget,
    state: Lock<State>,
}
impl<'m> Control<'m> {
    pub fn new(memory: &'m MemoryBudget, initial: Target) -> Result<Self, ControlError> {
        let snapshot = memory.snapshot();
        if snapshot.admission != initial.limits.admission
            || snapshot
                .categories
                .iter()
                .zip(initial.limits.categories)
                .any(|(category, cap)| category.limit != cap)
        {
            return Err(ControlError::InvalidInitialState);
        }
        Ok(Self {
            memory,
            state: Lock::new(State {
                last_published: initial,
                visible_floor: initial,
                pending: None,
                generation: 0,
                phase: Phase::StartupClosed,
                worker: WorkerState::Starting,
                claimed_generation: 0,
                socket: None,
            }),
        })
    }
    pub fn split(&mut self) -> Roles<'_, 'm> {
        Roles {
            acceptor: Acceptor { control: self },
            worker: Worker { control: self },
            refresher: Refresher { control: self },
            reader: Reader { control: self },
            stopper: Stopper { control: self },
        }
    }
}
pub struct Roles<'c, 'm> {
    pub acceptor: Acceptor<'c, 'm>,
    pub worker: Worker<'c, 'm>,
    pub refresher: Refresher<'c, 'm>,
    pub reader: Reader<'c, 'm>,
    pub stopper: Stopper<'c, 'm>,
}
pub struct Acceptor<'c, 'm> {
    control: &'c Control<'m>,
}
pub struct Worker<'c, 'm> {
    control: &'c Control<'m>,
}
pub struct Refresher<'c, 'm> {
    control: &'c Control<'m>,
}
#[derive(Clone, Copy)]
pub struct Reader<'c, 'm> {
    control: &'c Control<'m>,
}
#[derive(Clone, Copy)]
pub struct Stopper<'c, 'm> {
    control: &'c Control<'m>,
}

impl Reader<'_, '_> {
    pub fn snapshot(self) -> View {
        let state = self.control.state.lock();
        View {
            target: state.visible_floor,
            generation: state.generation,
            phase: state.phase,
            worker: state.worker,
        }
    }
}
impl Stopper<'_, '_> {
    pub fn stop(self) -> Option<TcpStream> {
        let mut state = self.control.state.lock();
        state.phase = Phase::Stopping;
        state.socket.take()
    }
}
impl Acceptor<'_, '_> {
    pub fn claim(&mut self, socket: TcpStream) -> Result<u64, Rejected> {
        let mut state = self.control.state.lock();
        let rejected = if state.phase == Phase::Stopping {
            Some(Reject::Stopping)
        } else if state.visible_floor.pressure || state.phase == Phase::Pressure {
            Some(Reject::Pressure)
        } else if state.phase != Phase::Enabled
            || state.worker != WorkerState::Idle
            || state.socket.is_some()
        {
            Some(Reject::Busy)
        } else {
            None
        };
        if let Some(reason) = rejected {
            return Err(Rejected { reason, socket });
        }
        state.worker = WorkerState::Busy;
        state.claimed_generation = state.generation;
        state.socket = Some(socket);
        Ok(state.claimed_generation)
    }
}
#[derive(Debug)]
pub enum Work {
    Socket { socket: TcpStream, generation: u64 },
    Stop(Option<TcpStream>),
    Empty,
}
impl Worker<'_, '_> {
    pub fn ready(&mut self) -> Result<(), ControlError> {
        let mut state = self.control.state.lock();
        if state.phase == Phase::Stopping {
            return Err(ControlError::Stopped);
        }
        if state.worker != WorkerState::Starting {
            state.phase = Phase::Stopping;
            return Err(ControlError::InvalidTransition);
        }
        state.worker = WorkerState::Idle;
        if state.phase == Phase::StartupClosed {
            state.phase = if state.visible_floor.pressure {
                Phase::Pressure
            } else {
                Phase::Enabled
            };
        }
        Ok(())
    }
    pub fn take(&mut self) -> Work {
        let mut state = self.control.state.lock();
        if state.phase == Phase::Stopping {
            return Work::Stop(state.socket.take());
        }
        match state.socket.take() {
            Some(socket) => Work::Socket {
                socket,
                generation: state.claimed_generation,
            },
            None => Work::Empty,
        }
    }
    pub fn complete_cleanup(&mut self) -> Result<(), ControlError> {
        let mut state = self.control.state.lock();
        if state.worker != WorkerState::Busy || state.socket.is_some() {
            state.phase = Phase::Stopping;
            return Err(ControlError::InvalidTransition);
        }
        state.worker = if state.phase == Phase::Stopping {
            WorkerState::Stopped
        } else {
            WorkerState::Idle
        };
        Ok(())
    }
    pub fn ack_stopped(&mut self) -> Result<(), ControlError> {
        let mut state = self.control.state.lock();
        if state.phase != Phase::Stopping
            || state.socket.is_some()
            || state.worker == WorkerState::StartupFailed
        {
            state.phase = Phase::Stopping;
            return Err(ControlError::InvalidTransition);
        }
        state.worker = WorkerState::Stopped;
        Ok(())
    }
    pub fn startup_failed(&mut self) -> Result<(), ControlError> {
        let mut state = self.control.state.lock();
        state.phase = Phase::Stopping;
        if state.worker != WorkerState::Starting || state.socket.is_some() {
            return Err(ControlError::InvalidTransition);
        }
        state.worker = WorkerState::StartupFailed;
        Ok(())
    }
}
pub struct Update<'r, 'c, 'm> {
    owner: &'r mut Refresher<'c, 'm>,
    target: Target,
    generation: u64,
}
impl<'c, 'm> Refresher<'c, 'm> {
    pub fn begin(&mut self, target: Target) -> Result<Update<'_, 'c, 'm>, ControlError> {
        let generation = {
            let mut state = self.control.state.lock();
            if state.phase == Phase::Stopping {
                return Err(ControlError::Stopped);
            }
            let Some(generation) = state.generation.checked_add(1) else {
                state.phase = Phase::Stopping;
                return Err(ControlError::GenerationExhausted);
            };
            state.generation = generation;
            state.visible_floor = state.visible_floor.conservative(target);
            state.pending = Some(target);
            state.phase = Phase::Updating;
            generation
        };
        Ok(Update {
            owner: self,
            target,
            generation,
        })
    }
}
impl Update<'_, '_, '_> {
    pub fn apply<E>(self, evict: impl FnOnce() -> Result<(), E>) -> Result<Publication, E> {
        let control = self.owner.control;
        control.memory.set_limits(self.target.limits);
        evict()?;
        let mut state = control.state.lock();
        if state.phase == Phase::Stopping || state.generation != self.generation {
            return Ok(Publication::Stopped);
        }
        state.last_published = self.target;
        state.visible_floor = self.target;
        state.pending = None;
        state.phase = if self.target.pressure {
            Phase::Pressure
        } else if state.worker == WorkerState::Starting {
            Phase::StartupClosed
        } else {
            Phase::Enabled
        };
        Ok(Publication::Installed)
    }
}
