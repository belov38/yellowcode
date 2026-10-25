use super::{Control, Roles};
#[cfg(target_os = "linux")]
use crate::discovery::linux::{
    bounded::{self, Scratch},
    bounded_io::NativeBoundedReader,
};
#[cfg(not(target_os = "linux"))]
use crate::policy::history::ConfirmedSweep;
use crate::policy::history::{Key, Observation as Bound};
use crate::{
    BudgetError,
    discovery::Visibility,
    policy::{
        CONTROL_RESERVE,
        history::{History, Reduced},
        numeric,
    },
};
use std::{
    alloc::{Layout, alloc, dealloc},
    ptr::NonNull,
};

const KIB: usize = 1024;
const DESCRIPTORS: usize = 16 * KIB;
const DISCOVERY: usize = 1952 * KIB;
const CAPACITIES: [usize; 9] = [
    64 * KIB,
    512 * KIB,
    64 * KIB,
    64 * KIB,
    64 * KIB,
    64 * KIB,
    192 * KIB,
    128 * KIB,
    64 * KIB,
];
const DEPLOYMENT_KEY: &[u8] = b"YELLOWCODE_CONTAINER_MEMORY_BYTES";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LayoutInfo {
    pub descriptor_bytes: usize,
    pub backing_bytes: usize,
    pub padding_bytes: usize,
}
fn geometry(descriptor_bytes: usize) -> Result<LayoutInfo, BudgetError> {
    let padding_bytes = DESCRIPTORS
        .checked_sub(descriptor_bytes)
        .ok_or(BudgetError::Overflow)?;
    let backing_bytes = usize::try_from(CONTROL_RESERVE)
        .map_err(|_| BudgetError::Overflow)?
        .checked_sub(descriptor_bytes)
        .ok_or(BudgetError::Overflow)?;
    Layout::array::<u8>(backing_bytes).map_err(|_| BudgetError::Overflow)?;
    Ok(LayoutInfo {
        descriptor_bytes,
        backing_bytes,
        padding_bytes,
    })
}
fn concrete_layout() -> Result<LayoutInfo, BudgetError> {
    let sizes = [
        size_of::<ControlReserve>(),
        size_of::<Regions<'_>>(),
        size_of::<DiscoveryBuffers<'_>>(),
        size_of::<DiscoverySession<'_>>(),
        size_of::<numeric::Startup>(),
        size_of::<numeric::Policy>(),
        size_of::<numeric::RawRefs<'_>>(),
        size_of::<numeric::RawRefs<'_>>(),
        size_of::<Reduced>(),
        size_of::<Observation>(),
        size_of::<LayoutInfo>(),
        size_of::<crate::policy::history::Tick<'_, '_>>(),
        #[cfg(target_os = "linux")]
        size_of::<NativeBoundedReader>(),
        size_of::<Control<'_>>(),
        size_of::<Roles<'_, '_>>(),
        #[cfg(target_os = "linux")]
        size_of::<Scratch<'_>>(),
        #[cfg(target_os = "linux")]
        size_of::<bounded::Summary>(),
    ];
    let d = sizes.into_iter().try_fold(0usize, |sum, n| {
        sum.checked_add(n).ok_or(BudgetError::Overflow)
    })?;
    geometry(d)
}

pub struct ControlReserve {
    pointer: NonNull<u8>,
    allocation: Layout,
    info: LayoutInfo,
}
unsafe impl Send for ControlReserve {}
impl ControlReserve {
    pub fn try_new() -> Result<Self, BudgetError> {
        let info = concrete_layout()?;
        let allocation =
            Layout::array::<u8>(info.backing_bytes).map_err(|_| BudgetError::Overflow)?;
        let pointer =
            NonNull::new(unsafe { alloc(allocation) }).ok_or(BudgetError::AllocationFailed)?;
        unsafe { pointer.as_ptr().write_bytes(0, info.backing_bytes) };
        Ok(Self {
            pointer,
            allocation,
            info,
        })
    }
    pub fn layout(&self) -> LayoutInfo {
        self.info
    }
    pub fn regions(&mut self) -> Regions<'_> {
        let rest = unsafe {
            std::slice::from_raw_parts_mut(self.pointer.as_ptr(), self.info.backing_bytes)
        };
        let (worker, rest) = rest.split_at_mut(64 * KIB);
        let (accept, rest) = rest.split_at_mut(16 * KIB);
        let (history_active, rest) = rest.split_at_mut(crate::policy::history::POOL_BYTES);
        let (history_scratch, rest) = rest.split_at_mut(crate::policy::history::POOL_BYTES);
        let (discovery, descriptor_padding) = rest.split_at_mut(DISCOVERY);
        Regions {
            worker,
            accept,
            history_active,
            history_scratch,
            discovery,
            descriptor_padding,
        }
    }
}
impl Drop for ControlReserve {
    fn drop(&mut self) {
        unsafe { dealloc(self.pointer.as_ptr(), self.allocation) };
    }
}
pub struct Regions<'a> {
    pub worker: &'a mut [u8],
    pub accept: &'a mut [u8],
    pub history_active: &'a mut [u8],
    pub history_scratch: &'a mut [u8],
    pub discovery: &'a mut [u8],
    pub descriptor_padding: &'a mut [u8],
}
struct DiscoveryBuffers<'a> {
    #[cfg_attr(
        not(target_os = "linux"),
        allow(dead_code, reason = "Reserved for the Linux discovery adapter")
    )]
    parts: [&'a mut [u8]; 9],
    _unused: &'a mut [u8],
}
impl<'a> DiscoveryBuffers<'a> {
    fn new(bytes: &'a mut [u8]) -> Result<Self, InvalidStorage> {
        if bytes.len() != DISCOVERY {
            return Err(InvalidStorage::Size);
        }
        let mut rest = bytes;
        let parts = CAPACITIES.map(|n| {
            let current = std::mem::take(&mut rest);
            let (part, tail) = current.split_at_mut(n);
            rest = tail;
            part
        });
        Ok(Self {
            parts,
            _unused: rest,
        })
    }
    #[cfg(target_os = "linux")]
    fn scratch(&mut self) -> Result<Scratch<'_>, InvalidStorage> {
        let [
            membership,
            mounts,
            root,
            mount,
            relative,
            text,
            key,
            provenance,
            component,
        ] = &mut self.parts;
        Scratch::new(
            membership, mounts, root, mount, relative, text, key, provenance, component,
        )
        .map_err(|_| InvalidStorage::Size)
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvalidStorage {
    Size,
    ZeroDeployment,
}
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SourceKinds {
    pub deployment: bool,
    pub cgroup_v2: bool,
    pub cgroup_v1: bool,
    pub physical: bool,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Observation {
    pub reduced: Reduced,
    pub visibility: Visibility,
    pub soft_limit: Option<u64>,
    pub sources: SourceKinds,
    #[cfg(target_os = "linux")]
    pub summary: bounded::Summary,
}
pub struct DiscoverySession<'a> {
    history: History<'a>,
    buffers: DiscoveryBuffers<'a>,
    deployment: Option<u64>,
}
impl<'a> DiscoverySession<'a> {
    pub fn new(
        active: &'a mut [u8],
        candidate: &'a mut [u8],
        discovery: &'a mut [u8],
        deployment: Option<u64>,
    ) -> Result<Self, InvalidStorage> {
        if deployment == Some(0) {
            return Err(InvalidStorage::ZeroDeployment);
        }
        if active.len() != crate::policy::history::POOL_BYTES
            || candidate.len() != crate::policy::history::POOL_BYTES
            || discovery.len() != DISCOVERY
        {
            return Err(InvalidStorage::Size);
        }
        let mut buffers = DiscoveryBuffers::new(discovery)?;
        #[cfg(target_os = "linux")]
        let _ = buffers.scratch()?;
        #[cfg(not(target_os = "linux"))]
        let _ = &mut buffers;
        let history = History::new(active, candidate).map_err(|_| InvalidStorage::Size)?;
        Ok(Self {
            history,
            buffers,
            deployment,
        })
    }
    pub fn discover(&mut self) -> Observation {
        #[cfg(target_os = "linux")]
        {
            self.discover_with(&NativeBoundedReader)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = &self.buffers;
            let mut tick = self.history.begin();
            if let Some(value) = self.deployment {
                tick.observe(Key::deployment(DEPLOYMENT_KEY), Bound::Finite(value));
            }
            let reduced = tick.finish_complete(ConfirmedSweep::new(
                b"deployment-only",
                b"deployment-only",
                true,
            ));
            Observation {
                reduced,
                visibility: Visibility::Unknown,
                soft_limit: None,
                sources: SourceKinds {
                    deployment: self.deployment.is_some(),
                    ..SourceKinds::default()
                },
            }
        }
    }
    #[cfg(target_os = "linux")]
    pub fn discover_with(&mut self, reader: &impl bounded::Reader) -> Observation {
        let mut tick = self.history.begin();
        if let Some(value) = self.deployment {
            tick.observe(Key::deployment(DEPLOYMENT_KEY), Bound::Finite(value));
        }
        let scratch = match self.buffers.scratch() {
            Ok(scratch) => scratch,
            Err(_) => {
                let reduced = tick.finish_partial();
                return Observation {
                    reduced,
                    visibility: Visibility::Partial,
                    soft_limit: None,
                    sources: SourceKinds {
                        deployment: self.deployment.is_some(),
                        ..SourceKinds::default()
                    },
                    summary: bounded::Summary {
                        warnings: bounded::CAPACITY,
                        ..Default::default()
                    },
                };
            }
        };
        let (reduced, summary) = bounded::discover(reader, scratch, tick);
        Observation {
            reduced,
            visibility: if reduced.partial {
                Visibility::Partial
            } else {
                Visibility::Visible
            },
            soft_limit: summary.soft_minimum,
            sources: SourceKinds {
                deployment: self.deployment.is_some(),
                cgroup_v2: summary.cgroup_v2,
                cgroup_v1: summary.cgroup_v1,
                physical: summary.physical_observed.is_some(),
            },
            summary,
        }
    }
}
