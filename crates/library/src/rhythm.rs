//! TEMPORARY STUB (replaced by the real module)
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, Default)]
pub struct Rhythm {
    pub bpm: f32,
    pub beat_confidence: f32,
    pub beats: Vec<f32>,
    pub downbeat: u32,
    pub key: u8,
    pub minor: bool,
    pub key_confidence: f32,
    pub energy: Vec<f32>,
}
pub const VERSION: i64 = 1;
pub fn analyze(_s: &[f32]) -> Rhythm {
    Rhythm::default()
}
impl Rhythm {
    pub fn camelot(&self) -> String {
        "8B".into()
    }
    pub fn to_blob(&self) -> Vec<u8> {
        vec![]
    }
    pub fn from_blob(_b: &[u8]) -> Option<Rhythm> {
        None
    }
}
