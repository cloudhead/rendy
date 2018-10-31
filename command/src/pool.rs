//! Pool module docs.

use ash::{
    version::DeviceV1_0,
    vk::{CommandBuffer, CommandPool, QueueFlags, CommandBufferAllocateInfo},
};

use failure::Error;
use relevant::Relevant;

use crate::{
    buffer::*,
    capability::*,
    family::FamilyIndex
};

/// Simple pool wrapper.
/// Doesn't provide any guarantees.
/// Wraps raw buffers into `Buffer`.
#[derive(Debug)]
pub struct Pool<C = QueueFlags, R = NoIndividualReset> {
    raw: CommandPool,
    capability: C,
    reset: R,
    family: FamilyIndex,
    relevant: Relevant,
}

impl<C, R> Pool<C, R>
where
    C: Capability,
    R: Reset,
{
    /// Wrap raw command pool.
    /// 
    /// # Safety
    /// 
    /// * `raw` must be valid command pool handle.
    /// * The command pool must be created for specified `family` index.
    /// * `capability` must be subset of capabilites of the `family` the pool was created for.
    /// * if `reset` is `IndividualReset` the pool must be created with individual command buffer reset flag set.
    pub unsafe fn from_raw(raw: CommandPool, capability: C, reset: R, family: FamilyIndex) -> Self {
        Pool {
            raw,
            capability,
            reset,
            family,
            relevant: Relevant,
        }
    }

    /// Allocate new command buffers.
    pub fn allocate_buffers<L: Level>(
        &mut self,
        device: &impl DeviceV1_0,
        level: L,
        count: usize,
    ) -> Vec<Buffer<C, InitialState, L, R>>
    where
        L: Level,
    {
        let buffers = unsafe {
            device.allocate_command_buffers(
                &CommandBufferAllocateInfo::builder()
                    .command_pool(self.raw)
                    .level(level.level())
                    .command_buffer_count(count as u32)
                    .build()
            )
        }.expect("Panic on OOM");

        buffers.into_iter().map(|raw| unsafe {
            Buffer::from_raw(
                raw,
                self.capability,
                InitialState,
                level,
                self.reset,
                self.family,
            )
        }).collect()
    }

    /// Free buffers.
    /// Buffers must be in droppable state.
    /// TODO: Validate buffers were allocated from this pool.
    pub fn free_buffers(
        &mut self,
        device: &impl DeviceV1_0,
        buffers: Vec<Buffer<C, impl Resettable, impl Level, R>>,
    ) {
        let buffers = buffers.iter().map(|buffer| unsafe { buffer.raw() }).collect::<Vec<_>>();
        unsafe {
            device.free_command_buffers(self.raw, &buffers);
        }
    }

    /// Reset all buffers of this pool.
    /// 
    /// # Safety
    /// 
    /// All buffers allocated from this pool must be marked reset.
    /// See [`Buffer::mark_reset`](struct.Buffer.html#method.mark_reset)
    pub unsafe fn reset(&mut self, device: &impl DeviceV1_0) {
        device.reset_command_pool(self.raw, Default::default())
            .expect("Panic if OOM");
    }

    /// Dispose of command pool.
    /// 
    /// # Safety
    /// 
    /// * All buffers allocated from this pool must be [freed](#method.free_buffers).
    pub unsafe fn dispose(self, device: &impl DeviceV1_0) {
        device.destroy_command_pool(self.raw, None);
        self.relevant.dispose();
    }
}

impl<R> Pool<QueueFlags, R> {
    /// Convert capability level
    pub fn from_flags<C>(self) -> Result<Pool<C, R>, Self>
    where
        C: Capability,
    {
        if let Some(capability) = C::from_flags(self.capability) {
            Ok(Pool {
                raw: self.raw,
                capability,
                reset: self.reset,
                family: self.family,
                relevant: self.relevant,
            })
        } else {
            Err(self)
        }
    }
}

/// Command pool that owns allocated buffers.
/// It can be used to borrow buffers one by one.
/// All buffers will be reset together via pool.
/// Prior reset user must ensure all buffers are complete.
#[derive(Debug)]
pub struct OwningPool<C = QueueFlags, L = PrimaryLevel> {
    inner: Pool<C>,
    level: L,
    buffers: Vec<CommandBuffer>,
    next: usize,
}

impl<C, L> OwningPool<C, L>
where
    C: Capability,
    L: Level,
{
    /// Wrap simple pool into owning version.
    /// 
    /// # Safety
    /// 
    /// * All buffers allocated from this pool must be [freed](#method.free_buffers).
    pub unsafe fn from_inner(inner: Pool<C>, level: L) -> Self {
        OwningPool {
            inner,
            level,
            buffers: Vec::new(),
            next: 0,
        }
    }

    /// Reserve at least `count` buffers.
    /// Allocate if there are not enough unused buffers.
    pub fn reserve(&mut self, device: &impl DeviceV1_0, count: usize) {
        let total = self.next + count;
        if total >= self.buffers.len() {
            let add = total - self.buffers.len();

            // TODO: avoid Vec allocation.
            self.buffers.extend(unsafe {
                device.allocate_command_buffers(
                    &CommandBufferAllocateInfo::builder()
                        .command_pool(self.inner.raw)
                        .level(self.level.level())
                        .command_buffer_count(add as u32)
                        .build()
                )
            }.expect("Panic on OOM"));
        }
    }

    /// Acquire next unused command buffer from pool.
    /// 
    /// # Safety
    /// 
    /// * Acquired buffer must be [released](struct.Buffer#method.release) when no longer needed.
    pub fn acquire_buffer(
        &mut self,
        device: &impl DeviceV1_0,
    ) -> Buffer<C, InitialState, L> {
        self.reserve(device, 1);
        self.next += 1;
        unsafe {
            Buffer::from_raw(
                self.buffers[self.next - 1],
                self.inner.capability,
                InitialState,
                self.level,
                self.inner.reset,
                self.inner.family,
            )
        }
    }

    /// Reset all buffers at once.
    /// [`Pool::acquire_buffer`](#method.acquire_buffer) will reuse allocated buffers.
    ///
    /// # Safety
    ///
    /// * All buffers acquired from this pool must be released.
    /// * Commands in buffers must be [complete](struct.Buffer#method.complete).
    /// 
    /// Note.
    /// * Any primary buffer that references secondary buffer from this pool will be invalidated.
    pub unsafe fn reset(&mut self, device: &impl DeviceV1_0) {
        self.inner.reset(device);
        self.next = 0;
    }

    /// Dispose of command pool.
    /// 
    /// # Safety
    /// 
    /// Same as for [`Pool::reset`](#method.reset).
    pub unsafe fn dispose(mut self, device: &impl DeviceV1_0) {
        self.reset(device);
        if !self.buffers.is_empty() {
            device.free_command_buffers(self.inner.raw, &self.buffers);
        }

        self.inner.dispose(device);
    }
}

impl<L> OwningPool<QueueFlags, L> {
    /// Convert capability level.
    pub fn from_flags<C>(self) -> Result<OwningPool<C, L>, Self>
    where
        C: Capability,
    {
        match self.inner.from_flags::<C>() {
            Ok(inner) => Ok(OwningPool {
                inner,
                level: self.level,
                buffers: self.buffers,
                next: self.next,
            }),
            Err(inner) => Err(OwningPool {
                inner,
                level: self.level,
                buffers: self.buffers,
                next: self.next,
            }),
        }
    }
}
