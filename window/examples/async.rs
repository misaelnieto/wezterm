use ::window::*;
use failure::Fallible;
use std::any::Any;

struct MyWindow {
    allow_close: bool,
    cursor_pos: Point,
}

impl Drop for MyWindow {
    fn drop(&mut self) {
        eprintln!("MyWindow dropped");
    }
}

impl WindowCallbacks for MyWindow {
    fn can_close(&mut self) -> bool {
        eprintln!("can I close?");
        if self.allow_close {
            true
        } else {
            self.allow_close = true;
            false
        }
    }

    fn destroy(&mut self) {
        eprintln!("destroy was called!");
        Connection::get().unwrap().terminate_message_loop();
    }

    fn paint(&mut self, context: &mut dyn PaintContext) {
        // Pick a purple background color
        context.clear(Color::rgb(0x40, 0x20, 0x60));
    }

    fn resize(&mut self, dims: Dimensions) {
        eprintln!("resize {:?}", dims);
    }

    fn key_event(&mut self, key: &KeyEvent, ctx: &dyn WindowOps) -> bool {
        eprintln!("{:?}", key);
        ctx.set_cursor(Some(MouseCursor::Text));
        false
    }

    fn mouse_event(&mut self, event: &MouseEvent, ctx: &dyn WindowOps) {
        self.cursor_pos = event.coords;
        ctx.invalidate();
        ctx.set_cursor(Some(MouseCursor::Arrow));

        if event.kind == MouseEventKind::Press(MousePress::Left) {
            eprintln!("{:?}", event);
        }
    }

    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}

async fn spawn_window() -> Result<(), Box<dyn std::error::Error>> {
    let win = Window::new_window(
        "myclass",
        "the title",
        800,
        600,
        Box::new(MyWindow {
            allow_close: false,
            cursor_pos: Point::new(100, 200),
        }),
    )?;

    eprintln!("before show");
    win.show().await?;
    eprintln!("after show");
    win.apply(|myself, _win| {
        eprintln!("doing apply");
        if let Some(myself) = myself.downcast_ref::<MyWindow>() {
            eprintln!(
                "got myself; allow_close={}, cursor_pos:{:?}",
                myself.allow_close, myself.cursor_pos
            );
        }
        Ok(())
    })
    .await?;
    eprintln!("done with spawn_window");
    Ok(())
}

fn main() -> Fallible<()> {
    let conn = Connection::init()?;
    conn.spawn_task(async {
        eprintln!("running this async block");
        spawn_window().await.ok();
        eprintln!("end of async block");
    });
    conn.run_message_loop()
}
