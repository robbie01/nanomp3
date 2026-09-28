#[derive(Debug, Clone, Copy)]
pub struct Buffer<Storage> {
    storage: Storage,
    bottom: usize,
    top: usize
}

impl<Storage> Buffer<Storage> {
    pub const fn new(storage: Storage) -> Self {
        Self {
            storage,
            bottom: 0,
            top: 0
        }
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
}

impl<Storage: AsMut<[u8]>> Buffer<Storage> {
    pub fn reclaim(&mut self) {
        self.storage.as_mut().copy_within(self.bottom..self.top, 0);
        self.top -= self.bottom;
        self.bottom = 0;
    }

    pub fn remaining_capacity(&mut self) -> &mut [u8] {
        &mut self.storage.as_mut()[self.top..]
    }
}

impl<Storage: AsRef<[u8]>> Buffer<Storage> {
    pub fn expand(&mut self, amt: usize) {
        assert!(amt <= self.storage.as_ref().len() - self.top);
        self.top += amt;
    }

    pub fn data(&self) -> &[u8] {
        &self.storage.as_ref()[self.bottom..self.top]
    }
}