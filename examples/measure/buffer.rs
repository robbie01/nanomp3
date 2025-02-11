#[derive(Debug, Clone, Copy)]
pub struct Buffer<const CAP: usize> {
    storage: [u8; CAP],
    bottom: usize,
    top: usize
}

impl<const CAP: usize> Buffer<CAP> {
    pub const fn new() -> Self {
        Self {
            storage: [0; CAP],
            bottom: 0,
            top: 0
        }
    }

    pub fn reclaim(&mut self) {
        self.storage.copy_within(self.bottom..self.top, 0);
        self.top -= self.bottom;
        self.bottom = 0;
    }

    pub fn data(&self) -> &[u8] {
        &self.storage[self.bottom..self.top]
    }

    pub fn len(&self) -> usize {
        self.top - self.bottom
    }
    
    pub fn is_empty(&self) -> bool {
        self.top == self.bottom
    }

    pub fn consume(&mut self, amt: usize) {
        assert!(amt <= self.top - self.bottom);
        self.bottom += amt;
    }

    pub fn remaining_capacity(&mut self) -> &mut [u8] {
        &mut self.storage[self.top..]
    }

    pub fn expand(&mut self, amt: usize) {
        assert!(amt <= CAP-self.top);
        self.top += amt;
    }
}