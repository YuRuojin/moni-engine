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

interface ScriptSpec {
  source: string;
}

interface EntitySpec {
  name?: string;
  transform?: TransformSpec;
  sprite?: SpriteSpec;
  script?: ScriptSpec;
}

interface EntityView {
  readonly id: EntityId;
  readonly name: string;
  readonly transform: Transform;
  readonly sprite?: Sprite;
}

interface ScriptContext {
  readonly tick: number;
  readonly entity: EntityView;
  setTransform(transform: Transform): void;
  setSprite(sprite: SpriteSpec | null): void;
  spawn(spec?: EntitySpec): void;
  despawn(entityId?: EntityId): void;
  log(message: unknown): void;
}

declare const console: {
  log(message: unknown): void;
};
