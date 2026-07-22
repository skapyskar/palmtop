package dev.palmtop.spike;

import static org.junit.Assert.assertEquals;

import org.junit.Test;

/**
 * The fit math needs checking against known cases rather than eyeballing on a
 * device -- a wrong result looks like "the video is a bit small" or "there's
 * a sliver clipped off one edge", not a crash, so nothing would flag it.
 */
public class VideoFitTest {

    @Test
    public void widerContainerThanContentPillarboxes() {
        // A typical tall phone in landscape (2400x1080, ratio ~2.22) fitting a
        // 1920x1080 (ratio 1.78) desktop: height is binding, bars left/right.
        VideoFit.Size s = VideoFit.fit(2400, 1080, 1920, 1080);
        assertEquals(new VideoFit.Size(1920, 1080), s);
    }

    @Test
    public void tallerContainerThanContentLetterboxes() {
        // Container proportionally taller than the content -- width is
        // binding, bars top/bottom.
        VideoFit.Size s = VideoFit.fit(1000, 1000, 1920, 1080);
        // contentRatio = 1920*1000 = 1,920,000; containerRatio = 1000*1080 = 1,080,000
        // containerRatio < contentRatio -> width binds
        assertEquals(new VideoFit.Size(1000, 563), s);
    }

    @Test
    public void matchingRatioFillsWithNoBars() {
        VideoFit.Size s = VideoFit.fit(1920, 1080, 1280, 720);
        assertEquals(new VideoFit.Size(1920, 1080), s);
    }

    @Test
    public void exactContainerMatchIsUnchanged() {
        VideoFit.Size s = VideoFit.fit(1280, 720, 1280, 720);
        assertEquals(new VideoFit.Size(1280, 720), s);
    }

    @Test
    public void neverExceedsContainerBoundsInEitherDimension() {
        // A wide range of container/content combinations -- the fitted
        // rectangle must never be larger than the container on either axis,
        // which would mean part of the video got clipped off-screen.
        int[][] cases = {
            {2400, 1080, 1920, 1080}, {1000, 1000, 1920, 1080}, {1080, 2400, 1920, 1080},
            {800, 480, 3840, 2160}, {1, 1, 1920, 1080}, {1920, 1080, 1, 1},
        };
        for (int[] c : cases) {
            VideoFit.Size s = VideoFit.fit(c[0], c[1], c[2], c[3]);
            assertTrueWithMessage(s.width <= c[0], "width " + s.width + " > container " + c[0]);
            assertTrueWithMessage(s.height <= c[1], "height " + s.height + " > container " + c[1]);
        }
    }

    @Test
    public void preservesAspectRatioWithinRoundingError() {
        int containerW = 2400, containerH = 1080, contentW = 1920, contentH = 1080;
        VideoFit.Size s = VideoFit.fit(containerW, containerH, contentW, contentH);
        double contentRatio = (double) contentW / contentH;
        double resultRatio = (double) s.width / s.height;
        assertTrueWithMessage(Math.abs(contentRatio - resultRatio) < 0.01,
                "ratio drifted: content=" + contentRatio + " result=" + resultRatio);
    }

    @Test
    public void nonPositiveDimensionsFallBackToContainerRatherThanCrashing() {
        // Before the first layout pass, or before a VideoConfig has arrived,
        // one side of this call is legitimately unknown (0) -- must not throw.
        VideoFit.Size s = VideoFit.fit(0, 0, 1920, 1080);
        assertEquals(new VideoFit.Size(0, 0), s);

        VideoFit.Size s2 = VideoFit.fit(1920, 1080, 0, 0);
        assertEquals(new VideoFit.Size(1920, 1080), s2);
    }

    private static void assertTrueWithMessage(boolean condition, String message) {
        if (!condition) throw new AssertionError(message);
    }

    // ---- computePlacement (crop-to-ratio) ----

    @Test
    public void placementWithTargetEqualToNativeRatioNeverOversizesTheSurface() {
        // Passing the video's own ratio as the target is what "Best Fit"
        // does -- must degrade to plain fit(), with zero cropping.
        VideoFit.Placement p = VideoFit.computePlacement(2000, 1000, 1920, 1080, 1920, 1080);
        assertEquals(p.visibleWidth, p.surfaceWidth);
        assertEquals(p.visibleHeight, p.surfaceHeight);
    }

    @Test
    public void placementCropsTo4x3FromA16x9Source() {
        // Hand-derived: 1920x1080 source cropped to 4:3 keeps the full
        // 1080 height and crops width down to 1440 (1080*4/3). That 1440x1080
        // slice contain-fits an 800x1000 container at 800x600 (height-bound).
        // The surface must be oversized on the cropped (width) axis only:
        // 1920 * (800/1440) = 1066.67 -> 1067; height is exact at 600.
        VideoFit.Placement p = VideoFit.computePlacement(800, 1000, 1920, 1080, 4, 3);
        assertEquals(new VideoFit.Placement(1067, 600, 800, 600), p);
    }

    @Test
    public void placementCropsToSquareFromA16x9Source() {
        // 1920x1080 cropped to 1:1 keeps the full 1080 height (the square's
        // side), cropping width down to 1080. That 1080x1080 square
        // contain-fits a 2000x1000 container at 1000x1000 (height-bound,
        // large pillarbox bars either side). Surface width must be oversized:
        // 1920 * (1000/1080) = 1777.78 -> 1778; height stays exact at 1000.
        VideoFit.Placement p = VideoFit.computePlacement(2000, 1000, 1920, 1080, 1, 1);
        assertEquals(new VideoFit.Placement(1778, 1000, 1000, 1000), p);
    }

    @Test
    public void placementNeverMakesTheVisibleRectExceedTheContainer() {
        int[][] cases = {
            {800, 1000, 1920, 1080, 4, 3}, {2000, 1000, 1920, 1080, 1, 1},
            {2400, 1080, 1920, 1080, 16, 9}, {1080, 2400, 1920, 1080, 1, 1},
        };
        for (int[] c : cases) {
            VideoFit.Placement p = VideoFit.computePlacement(c[0], c[1], c[2], c[3], c[4], c[5]);
            assertTrueWithMessage(p.visibleWidth <= c[0], "visible width exceeds container");
            assertTrueWithMessage(p.visibleHeight <= c[1], "visible height exceeds container");
        }
    }

    @Test
    public void placementSurfaceIsNeverSmallerThanVisible() {
        // The surface is either equal to (no crop on that axis) or larger
        // than (cropped axis) the visible rect -- never smaller, or the
        // parent's clipping would reveal empty space instead of cropping.
        int[][] cases = {
            {800, 1000, 1920, 1080, 4, 3}, {2000, 1000, 1920, 1080, 1, 1},
            {2400, 1080, 1920, 1080, 16, 9},
        };
        for (int[] c : cases) {
            VideoFit.Placement p = VideoFit.computePlacement(c[0], c[1], c[2], c[3], c[4], c[5]);
            assertTrueWithMessage(p.surfaceWidth >= p.visibleWidth, "surface narrower than visible");
            assertTrueWithMessage(p.surfaceHeight >= p.visibleHeight, "surface shorter than visible");
        }
    }
}
