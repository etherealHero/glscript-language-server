use tracing::{Event, Subscriber};
use tracing_subscriber::{
    fmt::{FmtContext, FormatEvent, FormatFields, FormattedFields},
    registry::LookupSpan,
};

pub struct Formatter;

impl<S, N> FormatEvent<S, N> for Formatter
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: tracing_subscriber::fmt::format::Writer<'_>,
        event: &Event<'_>,
    ) -> std::fmt::Result {
        write!(
            writer,
            "{} {:>5} ",
            chrono::Local::now().format("%H:%M:%S.%6f"),
            event.metadata().level(),
        )?;

        struct Visitor<'a> {
            w: &'a mut dyn std::fmt::Write,
        }

        impl<'a> tracing::field::Visit for Visitor<'a> {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                let s = self;
                let _ = match field.name() {
                    "message" if format!("{value:?}") == "\"close\"" => Ok(()),
                    "message" => write!(s.w, " {value:?}"),
                    "time.busy" => write!(s.w, " glproxy:{:>7};", format!("{value:?}")),
                    "time.idle" => write!(s.w, " tsserver:{:>7};", format!("{value:?}")),
                    other => write!(s.w, " {other}={value:?}"),
                };
            }
        }

        let mut visitor = Visitor { w: &mut writer };
        event.record(&mut visitor);

        if let Some(scope) = ctx.event_scope() {
            for span in scope.from_root() {
                if let Some(fields) = span.extensions().get::<FormattedFields<N>>() {
                    write!(writer, " {}", fields)?;
                }
            }
        }

        writeln!(writer)
    }
}
