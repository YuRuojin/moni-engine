use crate::engine::{SpriteKind, World};

pub fn render_svg(world: &World) -> String {
    let mut output = String::new();
    output.push_str(&format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{}\" height=\"{}\" viewBox=\"0 0 {} {}\">",
        world.viewport.width, world.viewport.height, world.viewport.width, world.viewport.height
    ));
    output.push_str(&format!(
        "<rect width=\"100%\" height=\"100%\" fill=\"{}\"/>",
        escape_xml(&world.viewport.background)
    ));

    let mut sprites: Vec<_> = world
        .entities
        .values()
        .filter_map(|entity| entity.sprite.as_ref().map(|sprite| (entity, sprite)))
        .collect();
    sprites.sort_by_key(|(entity, sprite)| (sprite.layer, entity.id));

    for (entity, sprite) in sprites {
        let x = entity.transform.x;
        let y = entity.transform.y;
        let rotation = entity.transform.rotation.to_degrees();
        let color = escape_xml(&sprite.color);
        match sprite.kind {
            SpriteKind::Rectangle => output.push_str(&format!(
                "<rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" fill=\"{}\" transform=\"rotate({} {} {})\"/>",
                x - sprite.width / 2.0,
                y - sprite.height / 2.0,
                sprite.width,
                sprite.height,
                color,
                rotation,
                x,
                y
            )),
            SpriteKind::Circle => output.push_str(&format!(
                "<ellipse cx=\"{}\" cy=\"{}\" rx=\"{}\" ry=\"{}\" fill=\"{}\" transform=\"rotate({} {} {})\"/>",
                x,
                y,
                sprite.width / 2.0,
                sprite.height / 2.0,
                color,
                rotation,
                x,
                y
            )),
        }
    }
    output.push_str("</svg>\n");
    output
}

fn escape_xml(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            _ => escaped.push(character),
        }
    }
    escaped
}
