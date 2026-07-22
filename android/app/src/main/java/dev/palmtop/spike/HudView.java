package dev.palmtop.spike;

import android.content.Context;
import android.graphics.Canvas;
import android.graphics.Color;
import android.graphics.Paint;
import android.graphics.Typeface;
import android.view.View;

import java.util.Locale;

/**
 * Live latency overlay.
 *
 * Exists so a mode change can be judged by its effect rather than by how it
 * feels -- which is the trap this whole workstream came out of. Before any of
 * this existed, "the video lags" and "the video is fine" were the only two
 * available observations, and neither one tells you which pipeline stage to
 * look at.
 *
 * The end-to-end figure is prefixed `~` deliberately. It is capture-to-decoded,
 * derived through a clock offset that assumes symmetric network delay, and it
 * excludes the panel's own response time entirely (not measurable without
 * external hardware -- unchanged since Phase 0). Printing it as a bare number
 * would invite quoting it as glass-to-glass, which it is not.
 */
final class HudView extends View {
    private final Paint text = new Paint(Paint.ANTI_ALIAS_FLAG);
    private final Paint background = new Paint();
    private volatile String[] lines = { "measuring…" };
    private volatile boolean shown = false;

    HudView(Context context) {
        super(context);
        float density = getResources().getDisplayMetrics().density;
        text.setColor(Color.WHITE);
        text.setTextSize(12f * density);
        text.setTypeface(Typeface.MONOSPACE);
        background.setColor(Color.argb(170, 0, 0, 0));
        setVisibility(GONE);
    }

    /** Named to avoid colliding with {@link View#isShown()}, which means
     *  something else entirely (attached *and* visible in the hierarchy). */
    boolean isHudShown() {
        return shown;
    }

    void setShown(boolean shown) {
        this.shown = shown;
        setVisibility(shown ? VISIBLE : GONE);
    }

    void update(LatencyTracker.Stats s, String modeName, int width, int height, int fps) {
        if (!shown) return;
        String format = String.format(Locale.US, "%s  %dx%d@%d", modeName, width, height, fps);
        lines = s.valid
                ? new String[] {
                        String.format(Locale.US, "~e2e %d/%d ms p50/p95",
                                s.e2eP50 / 1000, s.e2eP95 / 1000),
                        String.format(Locale.US, "rtt %d ms   dec %d ms",
                                s.rttP50 / 1000, s.decodeP50 / 1000),
                        String.format(Locale.US, "drop %.1f%%", s.dropPercent),
                        format,
                }
                : new String[] { "measuring…", format };
        postInvalidate();
    }

    @Override
    protected void onDraw(Canvas canvas) {
        super.onDraw(canvas);
        String[] snapshot = lines; // volatile read once -- update() runs off-thread
        float density = getResources().getDisplayMetrics().density;
        float pad = 6f * density;
        float lineHeight = text.getTextSize() * 1.35f;

        float boxWidth = 0;
        for (String line : snapshot) {
            boxWidth = Math.max(boxWidth, text.measureText(line));
        }
        canvas.drawRect(0, 0, boxWidth + pad * 2, lineHeight * snapshot.length + pad * 2, background);

        float y = pad + text.getTextSize();
        for (String line : snapshot) {
            canvas.drawText(line, pad, y, text);
            y += lineHeight;
        }
    }
}
