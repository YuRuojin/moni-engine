function onTick(ctx: ScriptContext, dt: number): void {
  const transform = ctx.entity.transform;
  ctx.setTransform({
    x: transform.x + 60 * dt,
    y: transform.y,
    rotation: transform.rotation + Math.PI * 0.5 * dt,
  });

  if (ctx.tick % 60 === 0) {
    ctx.log(`player tick ${ctx.tick}`);
  }
}
