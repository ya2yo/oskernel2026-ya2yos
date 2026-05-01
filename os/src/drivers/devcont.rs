use core::ops::{Deref, DerefMut};

use smallvec::SmallVec;

/// A structure that contains all device drivers of a certain category.
pub struct DeviceContainer<D>(SmallVec<[D; 1]>);

impl<D> DeviceContainer<D> {
    /// Constructs the container from one device.
    pub fn from_one(dev: D) -> Self {
        Self(SmallVec::from_buf([dev]))
    }

    /// Takes one device out of the container (will remove it from the
    /// container).
    pub fn take_one(&mut self) -> Option<D> {
        self.0.pop()
    }
}

impl<D> Deref for DeviceContainer<D> {
    type Target = SmallVec<[D; 1]>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<D> DerefMut for DeviceContainer<D> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<D> Default for DeviceContainer<D> {
    fn default() -> Self {
        Self(Default::default())
    }
}
