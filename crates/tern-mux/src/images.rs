//! A session's kitty graphics images, kept by the daemon (which answers the
//! program) and sent on to the client that draws them. Where an image shows
//! is in the grid already (placeholder cells), so only the images travel.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tern_term::graphics::{Image, ImageStore, STORE_CAP, StoredImage, VirtualPlacement};

use crate::proto::{self, HandedImage, ImageData, ServerMsg};

/// What a client was last sent of an image.
#[derive(Clone, PartialEq)]
struct Sent {
    generation: u64,
    placements: Vec<(u32, u32, u32)>,
}

pub struct ImageSync {
    store: Arc<Mutex<ImageStore>>,
    /// Locked across a change's whole broadcast, so a client attaching
    /// meanwhile gets every change after the full set it was sent.
    sent: Mutex<HashMap<u32, Sent>>,
}

fn placements(img: &StoredImage) -> Vec<(u32, u32, u32)> {
    img.placements.iter().map(|p| (p.id, p.cols, p.rows)).collect()
}

fn data(image: u32, img: &StoredImage, pixels: bool) -> ImageData {
    ImageData {
        image,
        generation: img.generation,
        width: img.image.width,
        height: img.image.height,
        placements: placements(img),
        pixels: pixels.then(|| proto::pack_pixels(&img.image.rgba)),
    }
}

impl Default for ImageSync {
    fn default() -> Self {
        Self { store: Arc::new(Mutex::new(ImageStore::new(STORE_CAP))), sent: Mutex::new(HashMap::new()) }
    }
}

impl ImageSync {
    pub fn store(&self) -> &Arc<Mutex<ImageStore>> {
        &self.store
    }

    /// What changed since the last call, as messages for session `id`,
    /// handed to `send` while no other change or full set can go out.
    pub fn changes(&self, id: u64, mut send: impl FnMut(&ServerMsg)) {
        let mut sent = self.sent.lock().unwrap();
        let msgs = {
            let Ok(store) = self.store.lock() else { return };
            let mut msgs = Vec::new();
            let gone: Vec<u32> = sent.keys().filter(|k| !store.iter().any(|(i, _)| i == **k)).copied().collect();
            for k in &gone {
                sent.remove(k);
            }
            if !gone.is_empty() {
                msgs.push(ServerMsg::ImagesGone { id, images: gone });
            }
            for (image, img) in store.iter() {
                let now = Sent { generation: img.generation, placements: placements(img) };
                let old = sent.get(&image);
                if old == Some(&now) {
                    continue;
                }
                let pixels = old.is_none_or(|o| o.generation != now.generation);
                msgs.push(ServerMsg::Image { id, image: data(image, img, pixels) });
                sent.insert(image, now);
            }
            msgs
        };
        for m in &msgs {
            send(m);
        }
    }

    /// Every image, for a client that just attached.
    pub fn all(&self, id: u64, mut send: impl FnMut(ServerMsg)) {
        let _sent = self.sent.lock().unwrap();
        let Ok(store) = self.store.lock() else { return };
        for (image, img) in store.iter() {
            send(ServerMsg::Image { id, image: data(image, img, true) });
        }
    }

    /// Every image, for the daemon taking the session over.
    pub fn handed(&self) -> Vec<HandedImage> {
        let Ok(store) = self.store.lock() else { return Vec::new() };
        store
            .iter()
            .map(|(image, img)| HandedImage {
                image,
                number: store.number_of(image),
                width: img.image.width,
                height: img.image.height,
                placements: placements(img),
                pixels: proto::pack_pixels(&img.image.rgba),
            })
            .collect()
    }

    /// Takes the images another daemon handed over; bad ones are skipped.
    pub fn adopt(&self, images: Vec<HandedImage>) {
        let mut store = self.store.lock().unwrap();
        for h in images {
            let Ok(rgba) = proto::unpack_pixels(&h.pixels) else { continue };
            if rgba.len() as u64 != 4 * u64::from(h.width) * u64::from(h.height) {
                continue;
            }
            if store.insert(h.image, Image { width: h.width, height: h.height, rgba: rgba.into() }).is_err() {
                continue;
            }
            if let Some(n) = h.number {
                store.set_number(n, h.image);
            }
            for (id, cols, rows) in h.placements {
                let _ = store.place(h.image, VirtualPlacement { id, cols, rows });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handed_images_come_back_whole() {
        let old = ImageSync::default();
        {
            let mut s = old.store().lock().unwrap();
            s.insert(5, image(1)).unwrap();
            s.set_number(9, 5);
            s.place(5, VirtualPlacement { id: 2, cols: 3, rows: 1 }).unwrap();
            s.insert(6, image(2)).unwrap();
        }
        let mut handed = old.handed();
        handed.push(HandedImage { image: 7, number: None, width: 2, height: 2, placements: vec![], pixels: proto::pack_pixels(&[0; 4]) });
        let new = ImageSync::default();
        new.adopt(handed);
        let mut s = new.store().lock().unwrap();
        assert_eq!(s.len(), 2, "the image with too few pixels is skipped");
        assert_eq!(s.resolve(0, 9), Some(5));
        let img = s.get(5).unwrap();
        assert_eq!(&img.image.rgba[..], &[1, 1, 1, 255]);
        assert_eq!(img.placements, [VirtualPlacement { id: 2, cols: 3, rows: 1 }]);
    }

    fn image(n: u8) -> Image {
        Image { width: 1, height: 1, rgba: vec![n, n, n, 255].into() }
    }

    fn changes(sync: &ImageSync) -> Vec<ServerMsg> {
        let mut out = vec![];
        sync.changes(1, |m| out.push(m.clone()));
        out
    }

    #[test]
    fn sends_new_changed_and_gone_images_once() {
        let sync = ImageSync::default();
        sync.store().lock().unwrap().insert(5, image(1)).unwrap();
        let out = changes(&sync);
        assert!(matches!(&out[..], [ServerMsg::Image { image, .. }] if image.image == 5 && image.pixels.is_some()));
        assert!(changes(&sync).is_empty());

        // A placement alone sends no pixels.
        sync.store().lock().unwrap().place(5, VirtualPlacement { id: 0, cols: 2, rows: 1 }).unwrap();
        let out = changes(&sync);
        assert!(matches!(&out[..], [ServerMsg::Image { image, .. }] if image.pixels.is_none() && image.placements == [(0, 2, 1)]));

        sync.store().lock().unwrap().delete(5);
        assert!(matches!(&changes(&sync)[..], [ServerMsg::ImagesGone { images, .. }] if images == &[5]));
    }

    #[test]
    fn a_new_client_gets_every_image() {
        let sync = ImageSync::default();
        sync.store().lock().unwrap().insert(1, image(1)).unwrap();
        sync.store().lock().unwrap().insert(2, image(2)).unwrap();
        changes(&sync);
        let mut all = vec![];
        sync.all(1, |m| all.push(m));
        assert_eq!(all.len(), 2);
        let ServerMsg::Image { image, .. } = &all[0] else { panic!() };
        assert_eq!(proto::unpack_pixels(image.pixels.as_ref().unwrap()).unwrap().len(), 4);
    }
}
