//! Reading a `sensor_msgs/PointCloud2` into points that can be drawn.
//!
//! The message is a blob plus a description of how to read it: `point_step`
//! bytes per point, and a `fields` list naming each channel with its byte
//! offset and datatype. Nothing about the layout is fixed — `x`, `y` and `z`
//! are usually three floats at offsets 0, 4 and 8, but a driver is entitled to
//! interleave them with anything, in any order, at any width.
//!
//! The conversion is pure and tested; nothing here touches the GPU.

use rw_canonical::CanonicalValue;

/// How many points are handed on to the renderer.
///
/// A spinning lidar publishes a few hundred thousand points per sweep and a
/// depth camera over three hundred thousand per frame, which draw fine. Beyond
/// this the cloud is thinned rather than dropped: a subsampled cloud still
/// shows the shape of the room, and a refused one shows nothing.
pub const BUDGET: usize = 400_000;

/// The datatypes `sensor_msgs/PointField` defines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Datatype {
    Int8,
    Uint8,
    Int16,
    Uint16,
    Int32,
    Uint32,
    Float32,
    Float64,
}

impl Datatype {
    fn from_code(code: u64) -> Option<Self> {
        Some(match code {
            1 => Self::Int8,
            2 => Self::Uint8,
            3 => Self::Int16,
            4 => Self::Uint16,
            5 => Self::Int32,
            6 => Self::Uint32,
            7 => Self::Float32,
            8 => Self::Float64,
            _ => return None,
        })
    }

    pub fn size(self) -> usize {
        match self {
            Self::Int8 | Self::Uint8 => 1,
            Self::Int16 | Self::Uint16 => 2,
            Self::Int32 | Self::Uint32 | Self::Float32 => 4,
            Self::Float64 => 8,
        }
    }

    /// Reads one value as an `f32`, which is what both the geometry and the
    /// colour ramp want. Integer channels keep their face value rather than
    /// being normalised: an intensity of 3000 means 3000.
    fn read(self, bytes: &[u8], big_endian: bool) -> Option<f32> {
        macro_rules! read {
            ($ty:ty, $n:expr) => {{
                let raw: [u8; $n] = bytes.get(..$n)?.try_into().ok()?;
                if big_endian {
                    <$ty>::from_be_bytes(raw) as f32
                } else {
                    <$ty>::from_le_bytes(raw) as f32
                }
            }};
        }
        Some(match self {
            Self::Int8 => *bytes.first()? as i8 as f32,
            Self::Uint8 => *bytes.first()? as f32,
            Self::Int16 => read!(i16, 2),
            Self::Uint16 => read!(u16, 2),
            Self::Int32 => read!(i32, 4),
            Self::Uint32 => read!(u32, 4),
            Self::Float32 => read!(f32, 4),
            Self::Float64 => read!(f64, 8),
        })
    }
}

/// One channel of the point record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Channel {
    pub name: String,
    pub offset: usize,
    pub datatype: Datatype,
}

/// A decoded cloud: positions, and whatever the message offered to colour them.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Cloud {
    pub points: Vec<[f32; 3]>,
    /// Per-point colour, when the message carries a packed `rgb`/`rgba` field.
    pub rgb: Option<Vec<[u8; 3]>>,
    /// Per-point scalar, when it carries `intensity` instead.
    pub intensity: Option<Vec<f32>>,
    /// How many points the message held, which is more than `points` whenever
    /// the cloud was thinned to fit the budget.
    pub total: usize,
}

impl Cloud {
    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }

    /// The axis-aligned box the points sit in, for framing the camera.
    pub fn bounds(&self) -> Option<([f32; 3], [f32; 3])> {
        let first = *self.points.first()?;
        let mut min = first;
        let mut max = first;
        for point in &self.points {
            for axis in 0..3 {
                min[axis] = min[axis].min(point[axis]);
                max[axis] = max[axis].max(point[axis]);
            }
        }
        Some((min, max))
    }

    /// The span of the intensity channel, so a colour ramp can be stretched
    /// across whatever range this sensor actually reports.
    pub fn intensity_range(&self) -> Option<(f32, f32)> {
        let values = self.intensity.as_ref()?;
        let mut min = f32::INFINITY;
        let mut max = f32::NEG_INFINITY;
        for value in values {
            if value.is_finite() {
                min = min.min(*value);
                max = max.max(*value);
            }
        }
        (min <= max).then_some((min, max))
    }
}

/// Reads a point cloud message, if this is one.
pub fn decode(value: &CanonicalValue) -> Option<Cloud> {
    let CanonicalValue::Struct(message) = value else {
        return None;
    };

    let channels = channels(message.get("fields")?)?;
    let point_step = number(message.get("point_step")?)?;
    let data = bytes(message.get("data")?)?;
    let big_endian = matches!(
        message.get("is_bigendian"),
        Some(CanonicalValue::Bool(true))
    );

    // `width * height` is the point count on paper, but a truncated `data` is
    // the thing that would actually be read past, so the blob decides.
    if point_step == 0 {
        return None;
    }
    let total = data.len() / point_step;
    if total == 0 {
        return None;
    }

    let find = |name: &str| channels.iter().find(|channel| channel.name == name);
    let (x, y, z) = (find("x")?, find("y")?, find("z")?);
    let intensity_channel = find("intensity");
    let rgb_channel = find("rgb").or_else(|| find("rgba"));

    // Thinning takes every nth point rather than the first n: a lidar sweep is
    // ordered by angle, so the first 400k points would be one side of the room.
    let stride = total.div_ceil(BUDGET).max(1);
    let kept = total.div_ceil(stride);

    let mut points = Vec::with_capacity(kept);
    let mut intensity = intensity_channel.map(|_| Vec::with_capacity(kept));
    let mut rgb = rgb_channel.map(|_| Vec::with_capacity(kept));

    for index in (0..total).step_by(stride) {
        let record = &data[index * point_step..(index + 1) * point_step];
        let read = |channel: &Channel| {
            channel
                .datatype
                .read(record.get(channel.offset..)?, big_endian)
        };
        let (Some(px), Some(py), Some(pz)) = (read(x), read(y), read(z)) else {
            continue;
        };
        // A cloud that is not `is_dense` marks missing returns with NaN or
        // infinity. Drawing those puts a point at the origin, or nowhere.
        if !px.is_finite() || !py.is_finite() || !pz.is_finite() {
            continue;
        }
        points.push([px, py, pz]);
        if let (Some(values), Some(channel)) = (intensity.as_mut(), intensity_channel) {
            values.push(read(channel).unwrap_or(0.));
        }
        if let (Some(colors), Some(channel)) = (rgb.as_mut(), rgb_channel) {
            colors.push(unpack_rgb(record, channel, big_endian));
        }
    }

    Some(Cloud {
        points,
        rgb,
        intensity,
        total,
    })
}

/// `rgb` is three bytes packed into a four-byte slot, conventionally as a float
/// whose bits are really an integer — so it is read as bytes, not as a number.
fn unpack_rgb(record: &[u8], channel: &Channel, big_endian: bool) -> [u8; 3] {
    let Some(slot) = record.get(channel.offset..channel.offset + 4) else {
        return [255, 255, 255];
    };
    if big_endian {
        [slot[1], slot[2], slot[3]]
    } else {
        [slot[2], slot[1], slot[0]]
    }
}

/// Reads the `fields` array: the message's own description of its layout.
fn channels(value: &CanonicalValue) -> Option<Vec<Channel>> {
    let CanonicalValue::Array(entries) = value else {
        return None;
    };
    let mut channels = Vec::with_capacity(entries.len());
    for entry in entries {
        let CanonicalValue::Struct(field) = entry else {
            continue;
        };
        let CanonicalValue::String(name) = field.get("name")? else {
            continue;
        };
        let offset = number(field.get("offset")?)?;
        let Some(datatype) = raw_number(field.get("datatype")?).and_then(Datatype::from_code)
        else {
            continue;
        };
        channels.push(Channel {
            name: name.clone(),
            offset,
            datatype,
        });
    }
    (!channels.is_empty()).then_some(channels)
}

fn number(value: &CanonicalValue) -> Option<usize> {
    usize::try_from(raw_number(value)?).ok()
}

fn raw_number(value: &CanonicalValue) -> Option<u64> {
    match value {
        CanonicalValue::Uint(inner) => Some(*inner),
        CanonicalValue::Int(inner) => u64::try_from(*inner).ok(),
        _ => None,
    }
}

fn bytes(value: &CanonicalValue) -> Option<Vec<u8>> {
    match value {
        CanonicalValue::Bytes(inner) => Some(inner.clone()),
        CanonicalValue::Array(items) => items
            .iter()
            .map(|item| match item {
                CanonicalValue::Uint(inner) => u8::try_from(*inner).ok(),
                CanonicalValue::Int(inner) => u8::try_from(*inner).ok(),
                _ => None,
            })
            .collect(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn field(name: &str, offset: u64, datatype: u64) -> CanonicalValue {
        let mut entry = BTreeMap::new();
        entry.insert("name".into(), CanonicalValue::String(name.into()));
        entry.insert("offset".into(), CanonicalValue::Uint(offset));
        entry.insert("datatype".into(), CanonicalValue::Uint(datatype));
        entry.insert("count".into(), CanonicalValue::Uint(1));
        CanonicalValue::Struct(entry)
    }

    fn message(fields: Vec<CanonicalValue>, point_step: u64, data: Vec<u8>) -> CanonicalValue {
        let mut message = BTreeMap::new();
        message.insert("fields".into(), CanonicalValue::Array(fields));
        message.insert("point_step".into(), CanonicalValue::Uint(point_step));
        message.insert("data".into(), CanonicalValue::Bytes(data));
        CanonicalValue::Struct(message)
    }

    /// Three floats per point, the layout almost every driver uses.
    fn xyz_only(points: &[[f32; 3]]) -> CanonicalValue {
        let mut data = Vec::new();
        for point in points {
            for axis in point {
                data.extend_from_slice(&axis.to_le_bytes());
            }
        }
        message(
            vec![field("x", 0, 7), field("y", 4, 7), field("z", 8, 7)],
            12,
            data,
        )
    }

    #[test]
    fn the_common_layout_decodes() {
        let cloud = decode(&xyz_only(&[[1., 2., 3.], [4., 5., 6.]])).expect("decodes");
        assert_eq!(cloud.points, vec![[1., 2., 3.], [4., 5., 6.]]);
        assert_eq!(cloud.total, 2);
        assert!(cloud.rgb.is_none() && cloud.intensity.is_none());
    }

    #[test]
    fn channels_are_read_from_the_message_not_assumed() {
        // z first, then x, then y, with a padding byte in front — legal, and
        // the reason offsets exist.
        let mut data = vec![0u8];
        data.extend_from_slice(&3f32.to_le_bytes());
        data.extend_from_slice(&1f32.to_le_bytes());
        data.extend_from_slice(&2f32.to_le_bytes());
        let cloud = decode(&message(
            vec![field("z", 1, 7), field("x", 5, 7), field("y", 9, 7)],
            13,
            data,
        ))
        .expect("decodes");
        assert_eq!(cloud.points, vec![[1., 2., 3.]]);
    }

    #[test]
    fn a_cloud_without_all_three_axes_is_not_a_cloud() {
        let mut data = Vec::new();
        data.extend_from_slice(&1f32.to_le_bytes());
        data.extend_from_slice(&2f32.to_le_bytes());
        assert_eq!(
            decode(&message(vec![field("x", 0, 7), field("y", 4, 7)], 8, data)),
            None
        );
    }

    #[test]
    fn intensity_comes_through_with_its_range() {
        let mut data = Vec::new();
        for (point, intensity) in [([0f32, 0., 0.], 10f32), ([1., 1., 1.], 250.)] {
            for axis in point {
                data.extend_from_slice(&axis.to_le_bytes());
            }
            data.extend_from_slice(&intensity.to_le_bytes());
        }
        let cloud = decode(&message(
            vec![
                field("x", 0, 7),
                field("y", 4, 7),
                field("z", 8, 7),
                field("intensity", 12, 7),
            ],
            16,
            data,
        ))
        .expect("decodes");
        assert_eq!(cloud.intensity, Some(vec![10., 250.]));
        assert_eq!(cloud.intensity_range(), Some((10., 250.)));
    }

    #[test]
    fn packed_rgb_is_read_as_bytes_rather_than_as_a_number() {
        let mut data = Vec::new();
        for axis in [0f32, 0., 0.] {
            data.extend_from_slice(&axis.to_le_bytes());
        }
        // Little-endian BGRx in the slot: blue 0x30, green 0x20, red 0x10.
        data.extend_from_slice(&[0x30, 0x20, 0x10, 0x00]);
        let cloud = decode(&message(
            vec![
                field("x", 0, 7),
                field("y", 4, 7),
                field("z", 8, 7),
                field("rgb", 12, 7),
            ],
            16,
            data,
        ))
        .expect("decodes");
        assert_eq!(cloud.rgb, Some(vec![[0x10, 0x20, 0x30]]));
    }

    #[test]
    fn integer_axes_are_read_at_their_own_width() {
        // x as int16, y as uint8, z as float64: nothing about this is unusual
        // enough for a decoder to refuse it.
        let mut data = Vec::new();
        data.extend_from_slice(&(-5i16).to_le_bytes());
        data.push(7);
        data.extend_from_slice(&2.5f64.to_le_bytes());
        let cloud = decode(&message(
            vec![field("x", 0, 3), field("y", 2, 2), field("z", 3, 8)],
            11,
            data,
        ))
        .expect("decodes");
        assert_eq!(cloud.points, vec![[-5., 7., 2.5]]);
    }

    #[test]
    fn big_endian_data_is_read_the_other_way_round() {
        let mut data = Vec::new();
        for axis in [1f32, 2., 3.] {
            data.extend_from_slice(&axis.to_be_bytes());
        }
        let CanonicalValue::Struct(mut message) = message(
            vec![field("x", 0, 7), field("y", 4, 7), field("z", 8, 7)],
            12,
            data,
        ) else {
            unreachable!()
        };
        message.insert("is_bigendian".into(), CanonicalValue::Bool(true));
        let cloud = decode(&CanonicalValue::Struct(message)).expect("decodes");
        assert_eq!(cloud.points, vec![[1., 2., 3.]]);
    }

    #[test]
    fn missing_returns_are_dropped_rather_than_drawn_at_the_origin() {
        let cloud = decode(&xyz_only(&[
            [1., 1., 1.],
            [f32::NAN, 0., 0.],
            [0., f32::INFINITY, 0.],
            [2., 2., 2.],
        ]))
        .expect("decodes");
        assert_eq!(cloud.points, vec![[1., 1., 1.], [2., 2., 2.]]);
        assert_eq!(cloud.total, 4, "the message still held four records");
    }

    #[test]
    fn a_truncated_blob_yields_only_the_points_it_holds() {
        // Three points' worth of fields, two points' worth of bytes.
        let mut data = Vec::new();
        for axis in [1f32, 2., 3., 4., 5., 6.] {
            data.extend_from_slice(&axis.to_le_bytes());
        }
        data.extend_from_slice(&7f32.to_le_bytes());
        let cloud = decode(&message(
            vec![field("x", 0, 7), field("y", 4, 7), field("z", 8, 7)],
            12,
            data,
        ))
        .expect("decodes");
        assert_eq!(cloud.points.len(), 2);
    }

    #[test]
    fn an_oversized_cloud_is_thinned_across_its_whole_extent() {
        let points: Vec<[f32; 3]> = (0..BUDGET * 2 + 10)
            .map(|index| [index as f32, 0., 0.])
            .collect();
        let cloud = decode(&xyz_only(&points)).expect("decodes");
        assert!(cloud.points.len() <= BUDGET, "thinned to fit the budget");
        assert_eq!(
            cloud.total,
            points.len(),
            "the true count is still reported"
        );
        let (min, max) = cloud.bounds().expect("has bounds");
        assert_eq!(min[0], 0.);
        assert!(
            max[0] > points.len() as f32 * 0.9,
            "the far end of the sweep survived thinning, got {}",
            max[0]
        );
    }

    #[test]
    fn a_zero_length_cloud_is_not_a_cloud() {
        assert_eq!(decode(&xyz_only(&[])), None);
    }

    #[test]
    fn an_unknown_datatype_is_ignored_rather_than_guessed() {
        let data = vec![0u8; 12];
        assert_eq!(
            decode(&message(
                vec![field("x", 0, 99), field("y", 4, 7), field("z", 8, 7)],
                12,
                data
            )),
            None,
            "x was dropped as unreadable, so there is no cloud"
        );
    }

    #[test]
    fn bounds_cover_every_axis() {
        let cloud = decode(&xyz_only(&[[-1., 5., 0.], [3., -2., 8.]])).expect("decodes");
        assert_eq!(cloud.bounds(), Some(([-1., -2., 0.], [3., 5., 8.])));
    }
}
