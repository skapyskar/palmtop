package dev.palmtop.spike;

import android.content.Context;
import android.graphics.Canvas;
import android.graphics.Color;
import android.graphics.Paint;
import android.graphics.Path;
import android.graphics.Point;
import android.view.View;

/**
 * Draws the live "found it" outline on top of the camera preview: a centred
 * framing guide while nothing is detected, and the actual detected QR
 * quadrilateral -- ML Kit's four corner points, not an axis-aligned bounding
 * box -- once one is. Green means it parsed as a palmtop pairing URI and is
 * about to be accepted; white means "that's a QR code, just not ours".
 *
 * The framing guide is purely cosmetic. Detection always runs over the whole
 * analysis frame, so a code sitting outside the guide still scans -- the guide
 * only nudges the user toward a distance where the code fills enough of the
 * sensor to be readable, which was the actual failure mode this whole overlay
 * was built to make visible.
 *
 * Coordinate mapping is the fiddly part, and getting it wrong is silent: the
 * box just floats somewhere near the code instead of on it. ML Kit reports
 * corner points in the coordinate space of the *rotated* analysis image
 * (because {@link QrScanActivity} hands {@code InputImage} the frame's
 * rotationDegrees), which is neither the same size nor necessarily the same
 * shape as this View. PreviewView's FILL_CENTER scale type centre-crops the
 * camera stream to fill the view, so reproducing exactly that transform --
 * scale by the *larger* of the two axis ratios, then centre -- is what keeps
 * the drawn outline pinned to the real code. QrScanActivity requests the same
 * aspect ratio for both Preview and ImageAnalysis so this one transform is
 * valid for both streams.
 */
final class QrOverlayView extends View {
    private static final int COLOR_MATCH = 0xFF4CD964;   // parsed as a palmtop:// URI
    private static final int COLOR_OTHER = 0xFFFFFFFF;   // a QR code, but not ours
    private static final int COLOR_GUIDE = 0x66FFFFFF;

    /** Fraction of the view's shorter side the framing guide spans. */
    private static final float GUIDE_FRACTION = 0.72f;

    private final Paint outline = new Paint(Paint.ANTI_ALIAS_FLAG);
    private final Paint fill = new Paint(Paint.ANTI_ALIAS_FLAG);
    private final Paint corner = new Paint(Paint.ANTI_ALIAS_FLAG);
    private final Paint guide = new Paint(Paint.ANTI_ALIAS_FLAG);
    private final Path path = new Path();

    /** Written from the analysis thread, read on the UI thread during draw. */
    private volatile Point[] corners;
    private volatile int srcW;
    private volatile int srcH;
    private volatile boolean matched;

    QrOverlayView(Context context) {
        super(context);
        setWillNotDraw(false);

        float density = getResources().getDisplayMetrics().density;

        outline.setStyle(Paint.Style.STROKE);
        outline.setStrokeWidth(3f * density);
        outline.setStrokeJoin(Paint.Join.ROUND);

        fill.setStyle(Paint.Style.FILL);

        corner.setStyle(Paint.Style.FILL);

        guide.setStyle(Paint.Style.STROKE);
        guide.setStrokeWidth(2f * density);
        guide.setStrokeCap(Paint.Cap.ROUND);
        guide.setColor(COLOR_GUIDE);
    }

    /**
     * @param corners ML Kit's four corner points in rotated-analysis-image space
     * @param srcW    width of that rotated analysis image
     * @param srcH    height of that rotated analysis image
     * @param matched whether the payload parsed as a palmtop pairing URI
     */
    void setDetection(Point[] corners, int srcW, int srcH, boolean matched) {
        if (corners == null || corners.length < 4 || srcW <= 0 || srcH <= 0) {
            clearDetection();
            return;
        }
        this.corners = corners;
        this.srcW = srcW;
        this.srcH = srcH;
        this.matched = matched;
        postInvalidate();
    }

    void clearDetection() {
        if (corners == null) return;
        corners = null;
        postInvalidate();
    }

    @Override
    protected void onDraw(Canvas canvas) {
        super.onDraw(canvas);
        Point[] pts = corners;
        if (pts == null) {
            drawGuide(canvas);
        } else {
            drawDetection(canvas, pts);
        }
    }

    private void drawGuide(Canvas canvas) {
        float w = getWidth(), h = getHeight();
        float side = Math.min(w, h) * GUIDE_FRACTION;
        float left = (w - side) / 2f, top = (h - side) / 2f;
        float right = left + side, bottom = top + side;
        float arm = side * 0.12f;

        // Four corner brackets rather than a full rectangle -- a closed box
        // reads as "the code must go inside here", which isn't true.
        drawBracket(canvas, left, top, arm, arm);
        drawBracket(canvas, right, top, -arm, arm);
        drawBracket(canvas, left, bottom, arm, -arm);
        drawBracket(canvas, right, bottom, -arm, -arm);
    }

    private void drawBracket(Canvas canvas, float x, float y, float dx, float dy) {
        canvas.drawLine(x, y, x + dx, y, guide);
        canvas.drawLine(x, y, x, y + dy, guide);
    }

    private void drawDetection(Canvas canvas, Point[] pts) {
        int accent = matched ? COLOR_MATCH : COLOR_OTHER;
        outline.setColor(accent);
        corner.setColor(accent);
        fill.setColor(Color.argb(matched ? 56 : 28, Color.red(accent), Color.green(accent), Color.blue(accent)));

        // FILL_CENTER: scale by the larger ratio so the stream covers the view,
        // then centre the overflow. Must match PreviewView exactly or the box
        // drifts off the code.
        float scale = Math.max((float) getWidth() / srcW, (float) getHeight() / srcH);
        float offX = (getWidth() - srcW * scale) / 2f;
        float offY = (getHeight() - srcH * scale) / 2f;

        path.reset();
        for (int i = 0; i < pts.length; i++) {
            float x = pts[i].x * scale + offX;
            float y = pts[i].y * scale + offY;
            if (i == 0) path.moveTo(x, y); else path.lineTo(x, y);
        }
        path.close();
        canvas.drawPath(path, fill);
        canvas.drawPath(path, outline);

        float dotRadius = 4f * getResources().getDisplayMetrics().density;
        for (Point p : pts) {
            canvas.drawCircle(p.x * scale + offX, p.y * scale + offY, dotRadius, corner);
        }
    }
}
