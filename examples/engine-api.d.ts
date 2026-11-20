type EntityId = number;

interface Transform {
  x: number;
  y: number;
  rotation: number;
}

interface TransformSpec {
  x: number;
  y: number;
  rotation?: number;
}

type SpriteKind = "rectangle" | "circle";

interface SpriteSpec {
  type: SpriteKind;
  width?: number;
  height?: number;
  color?: string;
  layer?: number;
}

interface Sprite {
  type: SpriteKind;
  width: number;
  height: number;
  color: string;
  layer: number;
}

interface Vector3 {
  x: number;
  y: number;
  z: number;
}

interface Transform3D {
  position: Vector3;
  rotation: Vector3;
  scale: Vector3;
}

interface Transform3DSpec {
  position?: Vector3;
  rotation?: Vector3;
  scale?: Vector3;
}

type MeshKind = "cube" | "plane" | "fbx";

interface Model3DSpec {
  type: MeshKind;
  source?: string;
  transform?: Transform3DSpec;
  color?: string;
  metallic?: number;
  roughness?: number;
}

interface Model3D {
  type: MeshKind;
  source?: string;
  transform: Transform3D;
  color: string;
  metallic: number;
  roughness: number;
}

type CameraProjection = "perspective" | "orthographic";

interface Camera3D {
  projection?: CameraProjection;
  orthographic_height?: number;
  position: Vector3;
  target: Vector3;
  fov_y: number;
  near: number;
  far: number;
}

interface ScriptSpec {
  source: string;
}

interface EntitySpec {
  name?: string;
  transform?: TransformSpec;
  sprite?: SpriteSpec;
  model?: Model3DSpec;
  script?: ScriptSpec;
}

interface EntityView {
  readonly id: EntityId;
  readonly name: string;
  readonly transform: Transform;
  readonly sprite?: Sprite;
  readonly model?: Model3D;
}

interface ScriptContext {
  readonly tick: number;
  readonly entity: EntityView;
  setTransform(transform: Transform): void;
  setSprite(sprite: SpriteSpec | null): void;
  setModel(model: Model3DSpec | null): void;
  setModelTransform(transform: Transform3D): void;
  setCamera(camera: Camera3D): void;
  spawn(spec?: EntitySpec): void;
  despawn(entityId?: EntityId): void;
  log(message: unknown): void;
}

declare const console: {
  log(message: unknown): void;
};
