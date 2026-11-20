use serde::{Deserialize, Serialize};

use crate::engine::EngineError;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Vector3 {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

impl Vector3 {
    pub const ZERO: Self = Self {
        x: 0.0,
        y: 0.0,
        z: 0.0,
    };

    pub const ONE: Self = Self {
        x: 1.0,
        y: 1.0,
        z: 1.0,
    };

    fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite() && self.z.is_finite()
    }
}

impl Default for Vector3 {
    fn default() -> Self {
        Self::ZERO
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Transform3d {
    #[serde(default)]
    pub position: Vector3,
    #[serde(default)]
    pub rotation: Vector3,
    #[serde(default = "unit_scale")]
    pub scale: Vector3,
}

fn unit_scale() -> Vector3 {
    Vector3::ONE
}

impl Default for Transform3d {
    fn default() -> Self {
        Self {
            position: Vector3::ZERO,
            rotation: Vector3::ZERO,
            scale: Vector3::ONE,
        }
    }
}

impl Transform3d {
    pub(crate) fn validate(&self) -> Result<(), EngineError> {
        if !self.position.is_finite() || !self.rotation.is_finite() || !self.scale.is_finite() {
            return Err(EngineError::new(
                "invalid_transform_3d",
                "3D transform values must be finite numbers",
            ));
        }
        if self.scale.x == 0.0 || self.scale.y == 0.0 || self.scale.z == 0.0 {
            return Err(EngineError::new(
                "invalid_transform_3d",
                "3D scale components must be non-zero",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MeshKind {
    Cube,
    Plane,
    Fbx,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Model3d {
    #[serde(rename = "type")]
    pub kind: MeshKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default)]
    pub transform: Transform3d,
    #[serde(default = "default_model_color")]
    pub color: String,
    #[serde(default)]
    pub metallic: f32,
    #[serde(default = "default_model_roughness")]
    pub roughness: f32,
}

fn default_model_roughness() -> f32 {
    0.5
}

fn default_model_color() -> String {
    "#ffffff".to_string()
}

impl Model3d {
    pub fn validate(&self) -> Result<(), EngineError> {
        self.transform.validate()?;
        if self.color.is_empty() || self.color.chars().count() > 64 {
            return Err(EngineError::new(
                "invalid_model_3d",
                "3D model color must contain between 1 and 64 characters",
            ));
        }
        if !self.metallic.is_finite()
            || !self.roughness.is_finite()
            || !(0.0..=1.0).contains(&self.metallic)
            || !(0.0..=1.0).contains(&self.roughness)
        {
            return Err(EngineError::new(
                "invalid_model_3d",
                "3D model metallic and roughness must be between 0 and 1",
            ));
        }
        match (&self.kind, &self.source) {
            (MeshKind::Fbx, Some(source))
                if !source.is_empty()
                    && source.len() <= 256
                    && source.to_ascii_lowercase().ends_with(".fbx") => {}
            (MeshKind::Fbx, _) => {
                return Err(EngineError::new(
                    "invalid_model_3d",
                    "FBX models require a workspace-relative .fbx source path",
                ));
            }
            (_, Some(_)) => {
                return Err(EngineError::new(
                    "invalid_model_3d",
                    "built-in meshes must not declare a source path",
                ));
            }
            (_, None) => {}
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Light {
    Directional {
        direction: Vector3,
        color: [f32; 3],
        intensity: f32,
        #[serde(default)]
        shadows: bool,
    },
    Point {
        position: Vector3,
        color: [f32; 3],
        intensity: f32,
        range: f32,
    },
}

impl Light {
    pub(crate) fn validate(&self) -> Result<(), EngineError> {
        let valid = match self {
            Self::Directional {
                direction,
                color,
                intensity,
                ..
            } => {
                direction.is_finite()
                    && *direction != Vector3::ZERO
                    && valid_light_color(color)
                    && valid_light_intensity(*intensity)
            }
            Self::Point {
                position,
                color,
                intensity,
                range,
            } => {
                position.is_finite()
                    && valid_light_color(color)
                    && valid_light_intensity(*intensity)
                    && range.is_finite()
                    && *range > 0.0
            }
        };
        if valid {
            Ok(())
        } else {
            Err(EngineError::new(
                "invalid_light",
                "light values must be finite, with a direction, non-negative color/intensity, and positive range",
            ))
        }
    }
}

fn valid_light_color(color: &[f32; 3]) -> bool {
    color
        .iter()
        .all(|channel| channel.is_finite() && *channel >= 0.0)
}

fn valid_light_intensity(intensity: f32) -> bool {
    intensity.is_finite() && intensity >= 0.0
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CameraProjection {
    #[default]
    Perspective,
    Orthographic,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Camera3d {
    #[serde(default)]
    pub projection: CameraProjection,
    #[serde(default = "default_orthographic_height")]
    pub orthographic_height: f64,
    #[serde(default = "default_camera_position")]
    pub position: Vector3,
    #[serde(default)]
    pub target: Vector3,
    #[serde(default = "default_camera_fov")]
    pub fov_y: f64,
    #[serde(default = "default_camera_near")]
    pub near: f64,
    #[serde(default = "default_camera_far")]
    pub far: f64,
}

fn default_orthographic_height() -> f64 {
    6.0
}

fn default_camera_position() -> Vector3 {
    Vector3 {
        x: 4.0,
        y: 3.0,
        z: 6.0,
    }
}

fn default_camera_fov() -> f64 {
    std::f64::consts::FRAC_PI_4
}

fn default_camera_near() -> f64 {
    0.1
}

fn default_camera_far() -> f64 {
    100.0
}

impl Default for Camera3d {
    fn default() -> Self {
        Self {
            projection: CameraProjection::Perspective,
            orthographic_height: default_orthographic_height(),
            position: default_camera_position(),
            target: Vector3::ZERO,
            fov_y: default_camera_fov(),
            near: default_camera_near(),
            far: default_camera_far(),
        }
    }
}

impl Camera3d {
    pub fn validate(&self) -> Result<(), EngineError> {
        if !self.position.is_finite()
            || !self.target.is_finite()
            || !self.fov_y.is_finite()
            || !self.orthographic_height.is_finite()
            || !self.near.is_finite()
            || !self.far.is_finite()
        {
            return Err(EngineError::new(
                "invalid_camera_3d",
                "3D camera values must be finite numbers",
            ));
        }
        if self.fov_y <= 0.0 || self.fov_y >= std::f64::consts::PI {
            return Err(EngineError::new(
                "invalid_camera_3d",
                "3D camera fov_y must be between 0 and PI radians",
            ));
        }
        if self.orthographic_height <= 0.0 {
            return Err(EngineError::new(
                "invalid_camera_3d",
                "3D camera orthographic_height must be positive",
            ));
        }
        if self.near <= 0.0 || self.far <= self.near {
            return Err(EngineError::new(
                "invalid_camera_3d",
                "3D camera requires near > 0 and far > near",
            ));
        }
        if self.position == self.target {
            return Err(EngineError::new(
                "invalid_camera_3d",
                "3D camera position and target must be different",
            ));
        }
        Ok(())
    }
}
