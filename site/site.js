(() => {
  const html = document.documentElement;
  const reducedMotion = () =>
    window.matchMedia && window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  const narrowQuery = window.matchMedia ? window.matchMedia("(max-width: 759px)") : null;
  const isNarrow = () => !!(narrowQuery && narrowQuery.matches);

  const clamp01 = (v) => Math.max(0, Math.min(1, v));
  const easeOut3 = (t) => 1 - Math.pow(1 - t, 3);

  /* ---------- Scroll reveal ---------- */
  // Reveals [data-reveal] elements as they enter the viewport, staggering them
  // in page order. An element is reset only once it is entirely
  // BELOW the viewport (the reader scrolled back up past it): the hidden state
  // moves it further down, so the reset can never pull it back into view and
  // flicker, and scrolling down again replays the animation.
  function initReveal() {
    window.plxReveal = true;
    if (reducedMotion() || !("IntersectionObserver" in window)) {
      html.classList.remove("reveal");
      return;
    }
    const targets = [...document.querySelectorAll("[data-reveal]")];
    if (!targets.length) return;

    // Each element starts 90ms after the one before it, including one that
    // arrived in an earlier observer batch, so a group crossing the line in
    // several batches still comes in top to bottom; but never more than
    // 180ms after it arrived, so the last of a long group does not sit
    // there looking disabled.
    const STAGGER_MS = 90;
    const MAX_DELAY_MS = 180;
    let lastStart = -Infinity;
    const reveal = (els) => {
      const now = performance.now();
      els.forEach((el) => {
        const delay = Math.min(MAX_DELAY_MS, Math.max(0, lastStart + STAGGER_MS - now));
        lastStart = now + delay;
        el.style.setProperty("--reveal-delay", Math.round(delay) + "ms");
        el.classList.add("is-revealed");
      });
    };

    const onEnter = (entries) =>
      reveal(
        entries
          .filter((e) => e.isIntersecting && !e.target.classList.contains("is-revealed"))
          .map((e) => e.target)
          .sort((x, y) => (x.compareDocumentPosition(y) & Node.DOCUMENT_POSITION_FOLLOWING ? -1 : 1))
      );
    const enter = new IntersectionObserver(onEnter, { rootMargin: "0px 0px -18% 0px", threshold: 0 });
    // Cards are big solid panels: waiting for the 82% line leaves a card-sized
    // blank on a phone screen that is then filled all at once, which reads as
    // a pop. They start as soon as their top is on screen (with a slower fade,
    // in styles.css).
    const enterCard = new IntersectionObserver(onEnter, { rootMargin: "0px 0px -2% 0px", threshold: 0 });

    const reset = new IntersectionObserver(
      (entries) => {
        entries.forEach((e) => {
          if (e.isIntersecting) return;
          const viewportBottom = e.rootBounds ? e.rootBounds.bottom : window.innerHeight;
          if (e.boundingClientRect.top >= viewportBottom) e.target.classList.remove("is-revealed");
        });
      },
      { threshold: 0 }
    );

    targets.forEach((el) => {
      (el.getAttribute("data-reveal") === "card" ? enterCard : enter).observe(el);
      reset.observe(el);
    });

    // The trigger line sits above the bottom of the viewport, so an element
    // near the end of the page could stay under it at full scroll on a tall
    // window. At the bottom, reveal whatever is on screen.
    const revealAtBottom = () => {
      if (window.innerHeight + window.scrollY < document.documentElement.scrollHeight - 2) return;
      reveal(
        targets.filter((el) => !el.classList.contains("is-revealed") && el.getBoundingClientRect().top < window.innerHeight)
      );
    };
    window.addEventListener("scroll", revealAtBottom, { passive: true });
    revealAtBottom();
  }

  /* ---------- Pointer-lit cards ---------- */
  // A light follows the mouse across [data-spot] cards and the card leans
  // slightly toward it. Touch and pen get the plain card.
  function initSpotlights() {
    if (reducedMotion()) return;
    document.querySelectorAll("[data-spot]").forEach((el) => {
      el.addEventListener("pointermove", (e) => {
        if (e.pointerType !== "mouse") return;
        const b = el.getBoundingClientRect();
        const x = (e.clientX - b.left) / b.width;
        const y = (e.clientY - b.top) / b.height;
        el.style.setProperty("--mx", (x * 100).toFixed(1) + "%");
        el.style.setProperty("--my", (y * 100).toFixed(1) + "%");
        el.style.setProperty("--spot", "1");
        el.style.transform =
          `perspective(1200px) rotateX(${((0.5 - y) * 5).toFixed(2)}deg) ` +
          `rotateY(${((x - 0.5) * 6).toFixed(2)}deg)`;
      });
      el.addEventListener("pointerleave", () => {
        el.style.setProperty("--spot", "0");
        el.style.transform = "";
      });
    });
  }

  /* ---------- Demo video ---------- */
  // Plays by itself as the TV scene comes up, like a product page loop. It is
  // on the TV's screen from the start: until it has a frame it shows its
  // poster, which is its own first frame, so starting playback swaps nothing
  // visible. The markup ships native controls so the video still works
  // without JS; with JS they are replaced by a single pause/play button,
  // always shown. Nothing is downloaded (preload="none") until the browser
  // has said it will play the video by itself (the probe in index.html's
  // head, window.plxAutoplay): where it will not (iOS Low Power Mode, a
  // browser set never to auto-play), the poster and the play button stay,
  // and a tap loads and starts the video in the same gesture. Reduced motion
  // starts paused, and a viewer's pause is never overridden.
  //
  // Returns { setInView(bool) }. The pinned TV scene calls setInView, since
  // it knows better than an IntersectionObserver when the video should run;
  // without the scene, an observer drives it.
  function initDemoVideo(sceneDriven) {
    const video = document.getElementById("demo-video");
    const card = document.getElementById("demo-card");
    const toggle = document.getElementById("demo-toggle");
    if (!video || !card || !toggle) return { setInView() {} };

    video.controls = false;
    video.muted = true;
    toggle.hidden = false;

    let userPaused = reducedMotion();
    let inView = false;
    // May the video start without a tap? Unknown (null) until the probe
    // answers; a tap on the button answers yes for this video.
    let mayAutoplay = null;
    let near = false;
    // Refused (the probe said no, or a play() was not allowed) and not yet
    // played: the button is the poster's own play button, centred and
    // larger (.is-start), until the video first plays.
    let refused = false;
    let started = false;

    const sync = () => {
      const playing = !video.paused;
      if (playing) started = true;
      toggle.dataset.state = playing ? "playing" : "paused";
      toggle.setAttribute("aria-label", playing ? "Pause video" : "Play video");
      toggle.classList.toggle("is-start", refused && !started);
    };
    const play = () => {
      const p = video.play();
      if (p && p.catch) {
        p.catch((e) => {
          if (e && e.name === "NotAllowedError") {
            refused = true;
            mayAutoplay = false;
          }
          sync();
        });
      }
    };
    const update = () => {
      if (inView && !userPaused && mayAutoplay) play();
      else if (!video.paused) video.pause();
    };
    // Buffer from two screens before the scene, so it is playing when it
    // arrives, but only for a video that will play by itself.
    const buffer = () => {
      if (near && mayAutoplay && !userPaused && video.preload !== "auto") video.preload = "auto";
    };

    if ("IntersectionObserver" in window) {
      const nearby = new IntersectionObserver(
        (entries) => {
          if (!entries.some((e) => e.isIntersecting)) return;
          nearby.disconnect();
          near = true;
          buffer();
        },
        { rootMargin: "200% 0px" }
      );
      nearby.observe(card);
    } else {
      near = true;
    }
    (window.plxAutoplay || Promise.resolve(true)).then((plays) => {
      if (mayAutoplay === null) {
        mayAutoplay = plays;
        refused = !plays;
      }
      sync();
      buffer();
      update();
    });

    video.addEventListener("play", sync);
    video.addEventListener("pause", sync);
    toggle.addEventListener("click", () => {
      userPaused = !video.paused;
      if (userPaused) {
        video.pause();
        return;
      }
      // The tap is the gesture every browser accepts, so it must reach
      // play() itself: a video that has loaded nothing starts loading here,
      // in the same handler. From now on it may also resume by itself.
      mayAutoplay = true;
      if (video.readyState === HTMLMediaElement.HAVE_NOTHING && video.networkState !== HTMLMediaElement.NETWORK_LOADING) {
        video.preload = "auto";
        video.load();
      }
      play();
    });
    sync();

    const setInView = (v) => {
      if (v === inView) return;
      inView = v;
      update();
    };

    if (!sceneDriven) {
      if ("IntersectionObserver" in window) {
        new IntersectionObserver((entries) => entries.forEach((e) => setInView(e.isIntersecting)), {
          threshold: 0.5,
        }).observe(card);
      } else {
        setInView(true);
      }
    }
    return { setInView };
  }

  /* ---------- Scroll-driven scenes ---------- */
  // Everything that moves with the scroll position rather than on a timer:
  // the hero receding, the pinned TV scene, the close-ups opening, the Why
  // sentence lighting up word by word, the quotes drifting and the big
  // headlines settling. One rAF-throttled pass per scroll frame.
  //
  // Two rules keep scrolling direct:
  // - Script never scrolls the page. A state that would look muddled if the
  //   reader stopped halfway through it (the glow, heading, stand and
  //   caption fading as the screen zooms) is a timed CSS
  //   transition that a scroll point triggers, so it always finishes by
  //   itself; only geometry (tilt, scale, position) follows the scroll, and
  //   it reads as intended wherever the reader stops. The scroll-snap glide
  //   this replaces scrolled the page after the reader let go and, on
  //   iPhone, ate the next swipe.
  // - A frame reads nothing but scrollY. Positions are measured when the
  //   layout changes (measure()), so style writes are never interleaved with
  //   layout reads, and no transform (the scene's own, or a reveal in
  //   progress) feeds back into what is measured.
  function initScenes() {
    const hero = document.querySelector(".hero");
    const pin = document.querySelector(".feel");
    const q = (sel) => (pin ? pin.querySelector(sel) : null);
    const sticky = q(".feel-sticky");
    const heading = q(".feel-heading");
    const stage = q(".feel-stage");
    const bezel = q(".feel-bezel");
    const screen = q(".demo-card");
    const toggle = q(".demo-toggle");
    const headerBar = document.querySelector(".site-header .header-bar");
    const frames = [...document.querySelectorAll(".closeup-box")].map((frame) => ({
      frame,
      img: frame.querySelector("img"),
      top: 0,
    }));
    const drifters = [...document.querySelectorAll("[data-drift]")].map((el) => ({
      el,
      k: Number(el.getAttribute("data-drift")) || 0,
      top: 0,
      h: 0,
    }));
    const bigs = [...document.querySelectorAll("[data-scrub-big]")].map((el) => ({ el, top: 0 }));
    const words = splitWords(document.querySelector("[data-words]"));

    const demo = initDemoVideo(true);

    // Measure the viewport once per width. On iPhone the toolbar collapses
    // mid-scroll and changes innerHeight; recomputing from it makes the
    // pinned scene jump.
    let vh = 0;
    let vhWidth = -1;
    const viewportHeight = () => {
      if (vhWidth !== window.innerWidth || !vh) {
        vhWidth = window.innerWidth;
        vh = document.documentElement.clientHeight || window.innerHeight;
      }
      return vh;
    };

    // Where the browser has CSS scroll-driven animations, the scene's
    // geometry (tilt, approach, zoom, the heading's rise) is animations on the
    // section's view timeline (styles.css), which the compositor advances in
    // step with the scroll. Script-driven, it trailed the page on iPhone,
    // where Safari scrolls in another process and updates the page late and
    // at most at 60Hz. This condition must match the @supports rule there.
    const cssDriven = !!(
      window.CSS &&
      CSS.supports &&
      CSS.supports("(animation-timeline: --a) and (animation-range: entry 0% entry 100%)")
    );

    // The layout a frame needs, measured only when the layout changes.
    const scene = !!(pin && sticky && stage && bezel && screen);
    const L = { vw: 0, pinTop: 0, pinH: 0, stickyTop: 0, stickyLeft: 0, stickyH: 0, stageLeft: 0, stageTop: 0 };
    L.cx = L.cy = L.wordsTop = L.wordsH = 0;
    L.screenW = L.full = 1;
    L.headerBottom = 0;
    // The TV stands in the page's flow (no pin, no zoom, the heading and
    // caption scrolling with the page) where the pinned scene cannot work,
    // has nothing to add, or would cost too much:
    //  - phones (narrow windows, either way up), where the column is nearly
    //    the window's width, and short landscape windows, where the heading,
    //    the TV and the caption cannot share the screen. styles.css lays
    //    these out by the same media query as flowQuery, and there the TV
    //    only rises in, on its own view timeline; without one it stands at
    //    rest, and no script draws it;
    //  - autoplay refused (iOS Low Power Mode: the probe in index.html's
    //    head) where the scene would be the per-frame fallback (.is-still):
    //    a low-power reader gets the poster and the play button without a
    //    script redrawing the scene on every scroll frame. Where the
    //    compositor runs the scene (cssDriven), it stays: that costs the
    //    page nothing.
    // Checked on every layout change, so turning a phone switches it.
    const flowQuery = window.matchMedia
      ? window.matchMedia("(max-width: 759px), (max-height: 559px) and (orientation: landscape)")
      : null;
    let refused = false;
    let still = false;
    const holdStill = () => {
      pin.classList.toggle("is-still", refused);
      const next = refused || !!(flowQuery && flowQuery.matches);
      if (next === still) return;
      still = next;
      if (!still) return;
      set(stage, "transform", "");
      set(stage, "opacity", "");
      if (toggle) set(toggle, "transform", "");
      if (heading) {
        set(heading, "opacity", "");
        set(heading, "transform", "");
      }
    };

    const measure = () => {
      L.vw = document.documentElement.clientWidth;
      if (scene) {
        holdStill();
        L.pinTop = docTop(pin);
        L.pinH = pin.offsetHeight;
        L.stickyTop = parseFloat(getComputedStyle(sticky).top) || 0;
        L.stickyLeft = sticky.getBoundingClientRect().left;
        L.stickyH = sticky.offsetHeight;
        L.stageLeft = stage.offsetLeft;
        L.stageTop = stage.offsetTop;
        L.cx = bezel.offsetLeft + screen.offsetLeft + screen.offsetWidth / 2;
        L.cy = bezel.offsetTop + screen.offsetTop + screen.offsetHeight / 2;
        L.screenW = Math.max(1, screen.offsetWidth);
        stage.style.transformOrigin = `${L.cx.toFixed(1)}px ${L.cy.toFixed(1)}px`;
        // Where the zoom ends: the screen as wide as the window, centred in
        // the band below the header capsule and clear of it by 8px or more
        // (in a window close to 16:9 that makes it a little narrower than
        // the window), while the heading and caption leave. Offsets are the
        // pinned box's own (sticky-relative) layout. The capsule is sticky,
        // so where it ends does not depend on the scroll position.
        L.headerBottom = headerBar ? headerBar.getBoundingClientRect().bottom : 0;
        const band = viewportHeight() - L.headerBottom - 16;
        L.full = Math.max(1, Math.min(L.vw / L.screenW, band / Math.max(1, screen.offsetHeight)));
        if (cssDriven) {
          // The zoom's end state for the CSS animations, from the pinned position.
          const zx = L.vw / 2 - (L.stickyLeft + L.stageLeft + L.cx);
          const zy = (viewportHeight() + L.headerBottom) / 2 - (L.stickyTop + L.stageTop + L.cy);
          stage.style.setProperty("--zoom-x", zx.toFixed(1) + "px");
          stage.style.setProperty("--zoom-y", zy.toFixed(1) + "px");
          stage.style.setProperty("--zoom-s", L.full.toFixed(4));
          stage.style.setProperty("--zoom-inv", (1 / L.full).toFixed(4));
        }
      }
      frames.forEach((f) => (f.top = docTop(f.frame)));
      drifters.forEach((d) => {
        d.top = docTop(d.el);
        d.h = d.el.offsetHeight;
      });
      bigs.forEach((b) => (b.top = docTop(b.el)));
      if (words.length) {
        const line = words[0].parentElement;
        L.wordsTop = docTop(line);
        L.wordsH = line.offsetHeight;
      }
    };

    if (scene && !cssDriven) {
      (window.plxAutoplay || Promise.resolve(true)).then((plays) => {
        if (plays) return;
        refused = true;
        measure();
        schedule();
      });
    }

    // will-change only while an element is near the screen (from half a
    // screen below it to half a screen past it), as apple.com does: a layer
    // held for every moving element all page long costs memory for nothing.
    if ("IntersectionObserver" in window) {
      const nearby = new IntersectionObserver(
        (entries) => entries.forEach((e) => e.target.classList.toggle("is-near", e.isIntersecting)),
        { rootMargin: "50% 0px" }
      );
      [pin, ...bigs.map((b) => b.el), ...frames.map((f) => f.frame.parentElement)].forEach((el) => el && nearby.observe(el));
    }

    let zoomed = false;
    let raf = 0;
    const tick = () => {
      raf = 0;
      const vh = viewportHeight();
      const vw = L.vw;
      const narrow = isNarrow();
      const y = window.scrollY;

      // The hero recedes: fades, drops a little and shrinks. No blur: a
      // filter redrawn every scroll frame over the whole hero cost frames.
      if (hero) {
        const p = clamp01(y / (vh * 0.7));
        set(hero, "opacity", (1 - p * 0.85).toFixed(3));
        set(hero, "transform", p > 0 ? `translateY(${(p * 60).toFixed(1)}px) scale(${(1 - p * 0.06).toFixed(4)})` : "");
      }

      if (scene) {
        const top = L.pinTop - y;
        const enter = easeOut3(clamp01((vh - top) / vh));
        const p = clamp01(-top / Math.max(1, L.pinH - vh));
        const ss = (a, b) => {
          const t = clamp01((p - a) / (b - a));
          return t * t * (3 - 2 * t);
        };
        // Progress through the pinned scene, p = 0..1, with the video playing
        // throughout (the same timeline as the CSS animations):
        //   0.00–0.12  the upright TV comes a little closer…
        //   0.12–0.52  …the screen grows to the window's width…
        //   0.52–0.70  …holds there…
        //   0.70–0.88  …and settles back, the heading, caption, stand and
        //              glow returning on the way (from about 0.85)…
        //   0.88–1.00  …to the whole scene at rest before the page moves on.
        // Every other stretch moves something, so the scene always answers
        // the scroll; a long still stretch read as the page not responding.
        const approach = 0.94 + 0.06 * ss(0, 0.12);
        const zoom = ss(0.12, 0.52) * (1 - ss(0.7, 0.88));

        // The timed state (styles.css, .feel.is-zoomed), tied to the zoom
        // itself: on once the screen is a sixth of the way to the window's
        // width (p ≈ 0.22), so the heading and caption do not leave an empty
        // band before it has grown into it; off again while it is still
        // settling (p ≈ 0.85), so they are back with the TV, not after it.
        // The gap between the two is hysteresis: resting on a threshold never
        // flickers.
        zoomed = !still && (zoomed ? zoom > 0.1 : zoom > 0.15);
        pin.classList.toggle("is-zoomed", zoomed);

        // The fallback: the same geometry as the CSS animations, per frame.
        if (!cssDriven && !still) {
          // Where the pinned box is: stuck at its `top`, or on its way in/out.
          const hostTop = Math.min(Math.max(top, L.stickyTop), top + L.pinH - L.stickyH);
          const dx = vw / 2 - (L.stickyLeft + L.stageLeft + L.cx);
          const dy = (vh + L.headerBottom) / 2 - (hostTop + L.stageTop + L.cy);
          const full = L.full;
          const scale = (0.8 + 0.2 * enter) * approach * (1 + (full - 1) * zoom);
          set(
            stage,
            "transform",
            `translate(${(dx * zoom).toFixed(1)}px, ${((1 - enter) * 60 + dy * zoom).toFixed(1)}px) ` +
              `rotateX(${((1 - enter) * 22).toFixed(2)}deg) scale(${scale.toFixed(4)})`
          );
          set(stage, "opacity", (0.3 + 0.7 * enter).toFixed(3));
          // Counter-scale so the pause button keeps its size while the TV zooms.
          if (toggle) set(toggle, "transform", `scale(${(1 / scale).toFixed(4)})`);
          // The heading rises in with the section; it leaves (and comes back)
          // with .is-zoomed.
          if (heading) {
            const hv = easeOut3(clamp01((vh - top) / (vh * 0.6)));
            set(heading, "opacity", hv.toFixed(3));
            set(heading, "transform", hv < 0.999 ? `translateY(${((1 - hv) * 40).toFixed(1)}px)` : "");
          }
        }
        // Play from half a screen before the scene comes up until half a
        // screen after it has gone, so the TV is already playing when it
        // appears.
        demo.setInView(top < vh * 1.5 && top + L.pinH > -vh * 0.5);
      }

      // Close-ups open from slightly smaller while the picture inside settles
      // from slightly larger. Nothing is cropped at rest. (Their glow comes
      // up with the picture's reveal: styles.css.)
      frames.forEach((f) => {
        const t = easeOut3(clamp01((vh - (f.top - y)) / (vh * 0.85)));
        const k = 1 - t;
        set(f.frame, "transform", k > 0.001 ? `scale(${(1 - 0.1 * k).toFixed(4)})` : "");
        if (f.img) set(f.img, "transform", k > 0.001 ? `scale(${(1 + 0.12 * k).toFixed(4)})` : "");
      });

      // Community quotes drift sideways a little at different rates, so the
      // group reads as layered. Off on phones, where they are one column.
      drifters.forEach((d) => {
        if (narrow) return set(d.el, "transform", "");
        const t = clamp01((vh - (d.top - y)) / (vh + d.h));
        set(d.el, "transform", `translateX(${((0.5 - t) * d.k * 40).toFixed(1)}px)`);
      });

      // Big headlines start large and settle into place. A scale, not a
      // font-size change, so their line breaks never move. Their fade is the
      // reveal's (a scroll fade on top of it faded them twice).
      bigs.forEach((b) => {
        const t = easeOut3(clamp01((vh - (b.top - y)) / (vh * 0.75)));
        const s0 = narrow ? 0.12 : 0.35;
        set(b.el, "transform", `scale(${(1 + s0 - s0 * t).toFixed(4)})`);
      });

      // The Why sentence lights up word by word in reading order: from when
      // its top is at 90% of the window's height until its bottom reaches
      // 62%, where it is being read, fully lit.
      if (words.length) {
        const lit = clamp01((vh * 0.9 - (L.wordsTop - y)) / (L.wordsH + vh * 0.28)) * words.length;
        words.forEach((w, i) => set(w, "opacity", (0.2 + 0.8 * clamp01(lit - i)).toFixed(3)));
      }
    };

    const schedule = () => {
      if (!raf) raf = requestAnimationFrame(tick);
    };
    window.addEventListener("scroll", schedule, { passive: true });
    onLayoutChange(() => {
      measure();
      schedule();
    });
    measure();
    tick();
  }

  // An element's top in document coordinates, from its layout box: offsetTop
  // ignores transforms, so neither a reveal in progress nor a scene's own
  // scaling moves what is measured.
  function docTop(el) {
    let t = 0;
    for (let e = el; e; e = e.offsetParent) t += e.offsetTop;
    return t;
  }

  // Writes one inline style property, skipping the write when the value has
  // not changed: most scroll frames leave most elements where they were.
  function set(el, prop, value) {
    const last = el.__plxStyle || (el.__plxStyle = {});
    if (last[prop] === value) return;
    last[prop] = value;
    el.style[prop] = value;
  }

  // Calls fn whenever the page's layout may have moved: resizes, the load
  // event, web fonts arriving, and any change in the page's size (images
  // decoding, lazy media, fonts reflowing text).
  function onLayoutChange(fn) {
    window.addEventListener("resize", fn);
    window.addEventListener("load", fn);
    if (document.fonts && document.fonts.ready) document.fonts.ready.then(fn);
    if ("ResizeObserver" in window) {
      new ResizeObserver(() => fn()).observe(document.querySelector(".page-root") || document.body);
    }
  }

  // Wraps each word of the Why sentence in a span so it can light up on its own.
  function splitWords(el) {
    if (!el) return [];
    const text = el.textContent.trim().replace(/\s+/g, " ");
    el.textContent = "";
    return text.split(" ").map((word, i, all) => {
      const span = document.createElement("span");
      span.className = "word";
      span.textContent = word;
      el.appendChild(span);
      if (i < all.length - 1) el.appendChild(document.createTextNode(" "));
      return span;
    });
  }

  /* ---------- Header ---------- */
  // The glass bar is dense by default (styles.css), so it reads over any page;
  // here, at the very top of the landing page with nothing under it yet, it
  // is marked .is-top and lightens. The section being read is marked: a pill
  // slides under its nav item, like the selection in the app's own tab bar,
  // and the Install button lights up in the install section. Section positions are measured on layout changes;
  // a scroll frame only compares scrollY with them. Not motion, so it runs
  // under reduced motion too (the pill then moves without sliding).
  function initHeader() {
    const header = document.querySelector(".site-header");
    if (!header) return;
    const nav = header.querySelector(".site-nav");
    const links = nav ? [...nav.querySelectorAll('a[href^="#"]')] : [];
    const cta = header.querySelector('.header-cta[href^="#"]');
    const items = [...links, cta]
      .filter((a) => a && document.getElementById(a.hash.slice(1)))
      .map((a) => ({ a, el: document.getElementById(a.hash.slice(1)), top: 0 }));

    let active = null;
    let atTop = null;
    let raf = 0;
    const placePill = (instant) => {
      if (!nav) return;
      const link = active && links.includes(active.a) ? active.a : null;
      if (link) {
        nav.style.setProperty("--pill-x", link.offsetLeft + "px");
        nav.style.setProperty("--pill-w", link.offsetWidth + "px");
      }
      // Appearing from nothing, the pill fades in where it belongs rather
      // than sliding over from wherever it was last.
      nav.classList.toggle("pill-instant", !!instant);
      nav.classList.toggle("has-pill", !!link);
    };
    const update = () => {
      raf = 0;
      const y = window.scrollY;
      if ((y <= 8) !== atTop) {
        atTop = y <= 8;
        header.classList.toggle("is-top", atTop);
      }
      // The section whose top has passed 40% of the way down the screen.
      const line = y + document.documentElement.clientHeight * 0.4;
      let next = null;
      items.forEach((it) => {
        if (it.top <= line) next = it;
      });
      if (next === active) return;
      const wasShown = !!(active && links.includes(active.a));
      if (active) {
        active.a.classList.remove("is-current");
        active.a.removeAttribute("aria-current");
      }
      active = next;
      if (active) {
        active.a.classList.add("is-current");
        active.a.setAttribute("aria-current", "true");
      }
      placePill(!wasShown);
    };
    const schedule = () => {
      if (!raf) raf = requestAnimationFrame(update);
    };
    const measure = () => {
      items.forEach((it) => (it.top = docTop(it.el)));
      items.sort((a, b) => a.top - b.top);
      placePill(true);
      schedule();
    };
    window.addEventListener("scroll", schedule, { passive: true });
    onLayoutChange(measure);
    // The first state now rather than a frame later, so a page opened at the
    // top is not seen fading from dense to light.
    atTop = window.scrollY <= 8;
    header.classList.toggle("is-top", atTop);
    measure();
  }

  initReveal();
  initSpotlights();
  initHeader();
  // Scroll scenes are motion by definition: under reduced motion the page
  // keeps its static layout (html.fx off) with an ordinary video.
  if (reducedMotion()) {
    html.classList.remove("fx");
    initDemoVideo(false);
  } else {
    html.classList.add("fx");
    initScenes();
  }
})();
