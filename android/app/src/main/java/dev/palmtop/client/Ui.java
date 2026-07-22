package dev.palmtop.client;

import android.content.Context;
import android.content.res.ColorStateList;
import android.graphics.Color;
import android.graphics.Typeface;
import android.graphics.drawable.Drawable;
import android.graphics.drawable.GradientDrawable;
import android.graphics.drawable.RippleDrawable;
import android.text.SpannableStringBuilder;
import android.text.Spanned;
import android.text.style.AbsoluteSizeSpan;
import android.text.style.ForegroundColorSpan;
import android.util.TypedValue;
import android.view.Gravity;
import android.view.View;
import android.widget.Button;
import android.widget.LinearLayout;
import android.widget.TextView;

/**
 * The app's design tokens and the small set of factories that build views from
 * them.
 *
 * <p>This exists because the UI is constructed entirely in Java -- there are no
 * XML layouts and no {@code res/values} styles to hold a design in. Without a
 * module like this, every colour, corner radius and text size is a literal at
 * whichever call site happened to need it, which is how the interface ended up
 * as raw {@code Button}s and {@code Color.GREEN} on {@code Color.BLACK}: not a
 * decision anyone made, just the platform defaults showing through in a dozen
 * places at once. One definition per token, referenced by name, is what makes
 * the look a thing that can be changed deliberately rather than hunted down.
 *
 * <h3>What the design is trying to be</h3>
 * The streamed laptop desktop is the content; this app is the instrument panel
 * next to it. So the chrome recedes: dark, low-contrast surfaces that do not
 * compete with whatever is on the laptop screen, one accent used sparingly for
 * things you can act on, and colour reserved almost entirely for connection
 * state -- where it carries real information.
 *
 * <p>Surfaces are a tonal ladder rather than one flat black. Pure black plus
 * the platform's default grey buttons is precisely what reads as unfinished,
 * and it also gives depth nothing to work with: with every surface identical,
 * a control panel and the letterbox bars behind it are the same colour. Three
 * steps ({@link #BASE} → {@link #PANEL} → {@link #RAISED}) separate "behind"
 * from "surface" from "you can press this" without needing shadows.
 */
final class Ui {

    private Ui() {}

    // ---------------------------------------------------------------- colour

    /** Furthest back: the window, and the letterbox/pillarbox bars around the
     *  video. Deliberately not pure black -- a true #000 next to a bright
     *  streamed desktop reads as a hole rather than as a surface, and on OLED
     *  it makes the panel edges disappear entirely. */
    static final int BASE = 0xFF0A0C0E;
    /** The control column and any full-screen sheet. */
    static final int PANEL = 0xFF14171B;
    /** Anything you can press. */
    static final int RAISED = 0xFF1E232A;
    /** Pressed state for {@link #RAISED}. Also the ripple colour. */
    static final int RAISED_PRESSED = 0xFF2C333C;
    /** 1px separators and control borders. Low contrast on purpose: it should
     *  define an edge without drawing attention to itself. */
    static final int HAIRLINE = 0xFF2A3038;

    static final int TEXT = 0xFFE8EDF2;
    /** Secondary copy, subtitles, units, and the resting status line. */
    static final int TEXT_MUTED = 0xFF97A3B0;
    /** Hints and disabled-ish text. */
    static final int TEXT_FAINT = 0xFF5E6A77;

    /** The one accent. Used for selection and the current value -- never as
     *  decoration. If everything is accented nothing is. */
    static final int ACCENT = 0xFF4C9FFF;

    // Status colours carry information, so they are the one place saturation
    // is allowed. Kept desaturated enough to sit on a dark panel without
    // vibrating against it, which straight Color.GREEN/Color.RED both do.
    static final int OK = 0xFF5BD98A;
    static final int WARN = 0xFFF2C55C;
    static final int ERR = 0xFFFF6B6B;

    /** Behind a full-screen sheet, so the video stays faintly visible and the
     *  sheet reads as temporary rather than as a different screen. */
    static final int SCRIM = 0xF20A0C0E;

    // ---------------------------------------------------------------- scale

    // A 4dp spacing scale. Named rather than numeric at the call site so
    // spacing stays consistent by construction instead of by memory.
    static int xs(Context c) { return dp(c, 4); }
    static int sm(Context c) { return dp(c, 8); }
    static int md(Context c) { return dp(c, 12); }
    static int lg(Context c) { return dp(c, 16); }
    static int xl(Context c) { return dp(c, 24); }

    static int dp(Context c, float value) {
        return Math.round(value * c.getResources().getDisplayMetrics().density);
    }

    /** Text sizes, in sp, as one ordered set. */
    static final float TEXT_TITLE = 17f;
    static final float TEXT_BODY = 13.5f;
    static final float TEXT_LABEL = 12.5f;
    static final float TEXT_SMALL = 11f;

    private static final float RADIUS_CONTROL = 10f;
    private static final float RADIUS_SHEET = 14f;

    /** Roboto Medium. The default weight is too light to hold a label against
     *  a dark surface at these sizes. */
    static Typeface medium() {
        return Typeface.create("sans-serif-medium", Typeface.NORMAL);
    }

    // ---------------------------------------------------------------- shapes

    static GradientDrawable rect(int fill, float radiusDp, Context c) {
        GradientDrawable d = new GradientDrawable();
        d.setColor(fill);
        d.setCornerRadius(dp(c, radiusDp));
        return d;
    }

    static GradientDrawable outlinedRect(int fill, int stroke, float radiusDp, Context c) {
        GradientDrawable d = rect(fill, radiusDp, c);
        d.setStroke(Math.max(1, dp(c, 1)), stroke);
        return d;
    }

    /**
     * A pressable background with real touch feedback.
     *
     * <p>{@link RippleDrawable} rather than a {@code StateListDrawable} colour
     * swap: the ripple originates at the finger and is the interaction users
     * already expect from every other Android app, so its absence is felt as
     * cheapness even when nobody can name what is missing.
     */
    static Drawable pressable(int fill, int stroke, float radiusDp, Context c) {
        GradientDrawable base = outlinedRect(fill, stroke, radiusDp, c);
        GradientDrawable mask = rect(Color.WHITE, radiusDp, c);
        return new RippleDrawable(ColorStateList.valueOf(RAISED_PRESSED), base, mask);
    }

    // ------------------------------------------------------------- factories

    /**
     * A control-column button: flat, left-aligned, sentence case.
     *
     * <p>Three platform defaults are undone here, and each one is individually
     * responsible for part of the "raw" look: buttons shout in ALL CAPS, they
     * carry a Material elevation shadow that fights a flat dark panel, and
     * their default minimum width/height are sized for a form, not a rail.
     */
    static Button button(Context c, String label) {
        Button b = new Button(c);
        b.setText(label);
        b.setAllCaps(false);
        b.setTextColor(TEXT);
        b.setTypeface(medium());
        b.setTextSize(TypedValue.COMPLEX_UNIT_SP, TEXT_LABEL);
        b.setGravity(Gravity.CENTER_VERTICAL | Gravity.START);
        b.setBackground(pressable(RAISED, HAIRLINE, RADIUS_CONTROL, c));
        b.setStateListAnimator(null); // kills the elevation lift on press
        b.setPadding(md(c), 0, md(c), 0);
        b.setMinHeight(dp(c, 42));
        b.setMinimumHeight(dp(c, 42));
        b.setMinWidth(0);
        b.setMinimumWidth(0);
        return b;
    }

    /** Icon-only variant: same surface, centred, square-ish. */
    static Button iconButton(Context c, String glyph) {
        Button b = button(c, glyph);
        b.setGravity(Gravity.CENTER);
        b.setPadding(0, 0, 0, 0);
        b.setTextSize(TypedValue.COMPLEX_UNIT_SP, 15f);
        return b;
    }

    /** The affirmative action on a sheet -- the one thing accented. */
    static Button primaryButton(Context c, String label) {
        Button b = button(c, label);
        b.setGravity(Gravity.CENTER);
        b.setTextColor(0xFF06121F);
        b.setBackground(pressable(ACCENT, ACCENT, RADIUS_CONTROL, c));
        return b;
    }

    /** A low-emphasis action: no fill, hairline edge. For "Close"/"Cancel",
     *  which should be reachable without competing with the real action. */
    static Button quietButton(Context c, String label) {
        Button b = button(c, label);
        b.setGravity(Gravity.CENTER);
        b.setTextColor(TEXT_MUTED);
        b.setBackground(pressable(Color.TRANSPARENT, HAIRLINE, RADIUS_CONTROL, c));
        return b;
    }

    static TextView title(Context c, String text) {
        TextView t = new TextView(c);
        t.setText(text);
        t.setTextColor(TEXT);
        t.setTypeface(medium());
        t.setTextSize(TypedValue.COMPLEX_UNIT_SP, TEXT_TITLE);
        return t;
    }

    static TextView body(Context c, String text) {
        TextView t = new TextView(c);
        t.setText(text);
        t.setTextColor(TEXT_MUTED);
        t.setTextSize(TypedValue.COMPLEX_UNIT_SP, TEXT_BODY);
        t.setLineSpacing(dp(c, 3), 1f);
        return t;
    }

    /** Monospace, for anything the eye scans as data rather than prose:
     *  addresses, resolutions, latency figures, log lines. Proportional type
     *  makes columns of numbers jitter as they update. */
    static TextView mono(Context c) {
        TextView t = new TextView(c);
        t.setTypeface(Typeface.MONOSPACE);
        t.setTextColor(TEXT_MUTED);
        t.setTextSize(TypedValue.COMPLEX_UNIT_SP, TEXT_SMALL);
        t.setLineSpacing(dp(c, 2), 1f);
        return t;
    }

    /**
     * A text field.
     *
     * <p>The platform's default {@code EditText} draws a single underline
     * tinted by the theme accent -- a Holo-era shape that reads as dated next
     * to filled controls, and which nearly vanishes on a dark panel. A filled
     * box with the same corner radius as the buttons puts inputs and controls
     * in the same family.
     */
    static android.widget.EditText input(Context c, String hint) {
        android.widget.EditText e = new android.widget.EditText(c);
        e.setHint(hint);
        e.setHintTextColor(TEXT_FAINT);
        e.setTextColor(TEXT);
        e.setTextSize(TypedValue.COMPLEX_UNIT_SP, TEXT_BODY);
        e.setBackground(outlinedRect(RAISED, HAIRLINE, RADIUS_CONTROL, c));
        e.setPadding(md(c), dp(c, 11), md(c), dp(c, 11));
        e.setSingleLine(true);
        return e;
    }

    /** A 1px rule. Used to separate a sheet's header from its content, where
     *  spacing alone leaves the header floating. */
    static View hairline(Context c) {
        View v = new View(c);
        v.setBackgroundColor(HAIRLINE);
        return v;
    }

    /** A full-screen sheet surface: scrim-dark, generous padding. Kept
     *  full-bleed rather than a floating card because the app runs locked to
     *  landscape on a phone, where a centred card would leave two useless
     *  gutters and less room for the content that matters. */
    static LinearLayout sheet(Context c) {
        LinearLayout l = new LinearLayout(c);
        l.setOrientation(LinearLayout.VERTICAL);
        l.setBackgroundColor(SCRIM);
        l.setPadding(xl(c), lg(c), xl(c), lg(c));
        // Sheets sit over live video and must swallow touches, or taps land on
        // the desktop behind them.
        l.setClickable(true);
        l.setFocusable(true);
        return l;
    }

    /** A selectable row (a saved laptop, a mode) rendered as a two-line card:
     *  the name in full-strength text, the detail beneath it muted and
     *  monospaced. One control, two levels of information, without needing a
     *  custom view. */
    static Button rowButton(Context c, String name, String detail, boolean selected) {
        Button b = button(c, "");
        SpannableStringBuilder sb = new SpannableStringBuilder();
        sb.append(name);
        if (detail != null && !detail.isEmpty()) {
            int start = sb.length();
            sb.append('\n').append(detail);
            sb.setSpan(new ForegroundColorSpan(TEXT_MUTED), start, sb.length(),
                    Spanned.SPAN_EXCLUSIVE_EXCLUSIVE);
            sb.setSpan(new AbsoluteSizeSpan((int) (TEXT_SMALL), true), start, sb.length(),
                    Spanned.SPAN_EXCLUSIVE_EXCLUSIVE);
        }
        b.setText(sb);
        b.setPadding(md(c), sm(c), md(c), sm(c));
        b.setMinHeight(dp(c, 52));
        b.setMinimumHeight(dp(c, 52));
        if (selected) {
            b.setTextColor(ACCENT);
            b.setBackground(pressable(RAISED, ACCENT, RADIUS_CONTROL, c));
        }
        return b;
    }

    /** Vertical gap between stacked children, applied as bottom margin so a
     *  caller never has to remember which side the gap belongs on. */
    static LinearLayout.LayoutParams stacked(Context c, int gapDp) {
        LinearLayout.LayoutParams lp = new LinearLayout.LayoutParams(
                LinearLayout.LayoutParams.MATCH_PARENT, LinearLayout.LayoutParams.WRAP_CONTENT);
        lp.bottomMargin = dp(c, gapDp);
        return lp;
    }

    static float radiusSheet() { return RADIUS_SHEET; }
}
