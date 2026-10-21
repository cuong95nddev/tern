//! A session's transmitted images and their virtual placements.

use std::collections::HashMap;

use super::Error;
use super::load::Image;

/// Most images kept, however small.
pub const MAX_IMAGES: usize = 1024;
/// Most virtual placements kept per image; the oldest go first.
pub const MAX_PLACEMENTS: usize = 32;

/// A placement shown through placeholder cells: the box, in cells, the image is fitted into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtualPlacement {
    pub id: u32,
    pub cols: u32,
    pub rows: u32,
}

#[derive(Debug)]
pub struct StoredImage {
    pub image: Image,
    /// Unique in the store; a replaced image gets a new one.
    pub generation: u64,
    pub placements: Vec<VirtualPlacement>,
    last_used: u64,
}

impl StoredImage {
    /// The placement a placeholder names, else the image's first one.
    pub fn placement(&self, id: u32) -> Option<VirtualPlacement> {
        self.placements.iter().find(|p| p.id == id).or_else(|| self.placements.first()).copied()
    }
}

#[derive(Debug)]
pub struct ImageStore {
    images: HashMap<u32, StoredImage>,
    /// Image numbers (`I=`) to the ids they were given.
    numbers: HashMap<u32, u32>,
    bytes: usize,
    cap: usize,
    clock: u64,
    next_id: u32,
}

impl ImageStore {
    /// A store holding at most `cap` bytes of decoded pixels.
    pub fn new(cap: usize) -> Self {
        Self { images: HashMap::new(), numbers: HashMap::new(), bytes: 0, cap, clock: 0, next_id: 0x7F00_0000 }
    }

    /// Decoded bytes held.
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// Images held.
    pub fn len(&self) -> usize {
        self.images.len()
    }

    pub fn is_empty(&self) -> bool {
        self.images.is_empty()
    }

    /// An unused id for an image transmitted without one.
    pub fn allocate_id(&mut self) -> u32 {
        loop {
            self.next_id = self.next_id.wrapping_add(1).max(1);
            if !self.images.contains_key(&self.next_id) {
                return self.next_id;
            }
        }
    }

    /// Names image `id` by `number`, replacing the number it had.
    pub fn set_number(&mut self, number: u32, id: u32) {
        self.numbers.retain(|_, v| *v != id);
        self.numbers.insert(number, id);
    }

    /// The number image `id` goes by, if any.
    pub fn number_of(&self, id: u32) -> Option<u32> {
        self.numbers.iter().find(|(_, v)| **v == id).map(|(n, _)| *n)
    }

    /// The stored image named by `id`, or by `number` when `id` is 0.
    pub fn resolve(&self, id: u32, number: u32) -> Option<u32> {
        let id = if id != 0 { id } else { *self.numbers.get(&number)? };
        self.images.contains_key(&id).then_some(id)
    }

    /// Stores `image` under `id`, replacing an earlier one and its placements.
    pub fn insert(&mut self, id: u32, image: Image) -> Result<(), Error> {
        if image.rgba.len() > self.cap {
            return Err(Error::new("EFBIG", "image too large"));
        }
        self.remove(id);
        self.clock += 1;
        self.bytes += image.rgba.len();
        self.images.insert(id, StoredImage { image, generation: self.clock, placements: Vec::new(), last_used: self.clock });
        self.evict(id);
        Ok(())
    }

    pub fn place(&mut self, id: u32, placement: VirtualPlacement) -> Result<(), Error> {
        let img = self.images.get_mut(&id).ok_or_else(|| Error::new("ENOENT", "no such image"))?;
        img.placements.retain(|p| p.id != placement.id);
        if img.placements.len() >= MAX_PLACEMENTS {
            img.placements.remove(0);
        }
        img.placements.push(placement);
        Ok(())
    }

    /// The image `id`, for drawing; marks it recently used.
    pub fn get(&mut self, id: u32) -> Option<&StoredImage> {
        self.clock += 1;
        let img = self.images.get_mut(&id)?;
        img.last_used = self.clock;
        Some(img)
    }

    /// Every image held, by id.
    pub fn iter(&self) -> impl Iterator<Item = (u32, &StoredImage)> {
        self.images.iter().map(|(id, img)| (*id, img))
    }

    pub fn delete(&mut self, id: u32) {
        self.remove(id);
        self.numbers.retain(|_, v| *v != id);
    }

    pub fn clear(&mut self) {
        self.images.clear();
        self.numbers.clear();
        self.bytes = 0;
    }

    fn remove(&mut self, id: u32) {
        if let Some(old) = self.images.remove(&id) {
            self.bytes -= old.image.rgba.len();
        }
    }

    fn evict(&mut self, keep: u32) {
        while self.bytes > self.cap || self.images.len() > MAX_IMAGES {
            let Some(victim) = self.images.iter().filter(|(id, _)| **id != keep).min_by_key(|(_, img)| img.last_used).map(|(id, _)| *id)
            else {
                break;
            };
            self.delete(victim);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img(px: usize) -> Image {
        Image { width: 1, height: px as u32, rgba: vec![0; 4 * px].into() }
    }

    #[test]
    fn replace_changes_generation_and_drops_placements() {
        let mut s = ImageStore::new(1 << 20);
        s.insert(1, img(1)).unwrap();
        s.place(1, VirtualPlacement { id: 0, cols: 2, rows: 1 }).unwrap();
        let first = s.get(1).unwrap().generation;
        s.insert(1, img(2)).unwrap();
        let again = s.get(1).unwrap();
        assert_ne!(again.generation, first);
        assert!(again.placements.is_empty());
        assert_eq!(s.bytes(), 8);
    }

    #[test]
    fn eviction_drops_the_least_recently_used() {
        let mut s = ImageStore::new(40);
        s.insert(1, img(4)).unwrap();
        s.insert(2, img(4)).unwrap();
        s.get(1);
        s.insert(3, img(4)).unwrap();
        assert!(s.get(2).is_none());
        assert!(s.get(1).is_some() && s.get(3).is_some());
        assert_eq!(s.bytes(), 32);
    }

    #[test]
    fn an_image_bigger_than_the_store_is_refused() {
        let mut s = ImageStore::new(40);
        s.insert(1, img(4)).unwrap();
        assert_eq!(s.insert(2, img(20)).unwrap_err().code, "EFBIG");
        assert!(s.get(2).is_none() && s.get(1).is_some());
    }

    #[test]
    fn image_count_is_capped() {
        let mut s = ImageStore::new(1 << 30);
        for id in 1..=MAX_IMAGES as u32 + 1 {
            s.insert(id, img(1)).unwrap();
        }
        assert_eq!(s.len(), MAX_IMAGES);
        assert!(s.get(1).is_none(), "the oldest went");
    }

    #[test]
    fn placements_are_capped_and_numbers_follow_the_image() {
        let mut s = ImageStore::new(1 << 20);
        s.insert(1, img(1)).unwrap();
        for p in 1..=100 {
            s.place(1, VirtualPlacement { id: p, cols: 1, rows: 1 }).unwrap();
        }
        let placements = &s.get(1).unwrap().placements;
        assert_eq!(placements.len(), MAX_PLACEMENTS);
        assert_eq!(placements.last().unwrap().id, 100);
        s.set_number(5, 1);
        s.set_number(6, 1);
        assert_eq!((s.resolve(0, 5), s.resolve(0, 6)), (None, Some(1)));
    }

    #[test]
    fn placements_and_numbers() {
        let mut s = ImageStore::new(1 << 20);
        assert_eq!(s.place(9, VirtualPlacement { id: 0, cols: 1, rows: 1 }).unwrap_err().code, "ENOENT");
        let id = s.allocate_id();
        s.insert(id, img(1)).unwrap();
        s.set_number(4, id);
        assert_eq!(s.resolve(0, 4), Some(id));
        s.place(id, VirtualPlacement { id: 3, cols: 5, rows: 2 }).unwrap();
        assert_eq!(s.get(id).unwrap().placement(3), Some(VirtualPlacement { id: 3, cols: 5, rows: 2 }));
        assert_eq!(s.get(id).unwrap().placement(0), Some(VirtualPlacement { id: 3, cols: 5, rows: 2 }));
        s.delete(id);
        assert_eq!(s.resolve(0, 4), None);
        assert_eq!(s.bytes(), 0);
    }
}
