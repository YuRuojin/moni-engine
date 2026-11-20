var onTick = function (ctx: ScriptContext, dt: number): void {
  const model = ctx.entity.model;
  if (!model) {
    return;
  }

  const transform = model.transform;
  ctx.setModelTransform({
    position: transform.position,
    rotation: {
      x: transform.rotation.x + 0.35 * dt,
      y: transform.rotation.y + 0.7 * dt,
      z: transform.rotation.z,
    },
    scale: transform.scale,
  });
};
