#[derive(Debug, Clone, Copy, PartialEq, Default, serde::Serialize)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Default, serde::Serialize)]
pub struct Size {
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect { x, y, w, h }
    }
    pub fn from_array(a: [f32; 4]) -> Rect {
        Rect::new(a[0], a[1], a[2], a[3])
    }
    pub fn to_array(self) -> [f32; 4] {
        [self.x, self.y, self.w, self.h]
    }
}
