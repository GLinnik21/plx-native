(() => {
  const html = document.documentElement;
  const reducedMotion = () =>
    window.matchMedia && window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  const narrowQuery = window.matchMedia ? window.matchMedia("(max-width: 759px)") : null;
  const isNarrow = () => !!(narrowQuery && narrowQuery.matches);

  const clamp01 = (v) => Math.max(0, Math.min(1, v));
  const easeOut3 = (t) => 1 - Math.pow(1 - t, 3);

  /* ---------- Scroll reveal ---------- */
  // Reveals [data-reveal] elements as they enter the viewport, staggering the
  // ones that enter together. An element is reset only once it is entirely
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

    const STAGGER_MS = 90;
    const MAX_DELAY_MS = 540;

    const enter = new IntersectionObserver(
      (entries) => {
        const entering = entries
          .filter((e) => e.isIntersecting && !e.target.classList.contains("is-revealed"))
          .map((e) => e.target)
          .sort((x, y) => (x.compareDocumentPosition(y) & Node.DOCUMENT_POSITION_FOLLOWING ? -1 : 1));
        entering.forEach((el, i) => {
          el.style.setProperty("--reveal-delay", Math.min(i * STAGGER_MS, MAX_DELAY_MS) + "ms");
          el.classList.add("is-revealed");
        });
      },
      { rootMargin: "0px 0px -18% 0px", threshold: 0 }
    );

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
      enter.observe(el);
      reset.observe(el);
    });

    // The trigger line sits above the bottom of the viewport, so an element
    // near the end of the page could stay under it at full scroll on a tall
    // window. At the bottom, reveal whatever is on screen.
    const revealAtBottom = () => {
      if (window.innerHeight + window.scrollY < document.documentElement.scrollHeight - 2) return;
      targets
        .filter((el) => !el.classList.contains("is-revealed") && el.getBoundingClientRect().top < window.innerHeight)
        .forEach((el, i) => {
          el.style.setProperty("--reveal-delay", Math.min(i * STAGGER_MS, MAX_DELAY_MS) + "ms");
          el.classList.add("is-revealed");
        });
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
  // Plays by itself while it is the thing on screen, like a product page
  // loop. The markup ships native controls so the video still works without
  // JS; with JS they are replaced by a single pause/play button. Reduced
  // motion starts paused, and a viewer's pause is never overridden.
  //
  // Returns { setInView(bool), shown() }. The pinned TV scene calls
  // setInView, since it knows better than an IntersectionObserver when the
  // video should run; without the scene, an observer drives it. shown() ramps
  // 0→1 over a moment once the first frame is actually painted, so the scene
  // never fades in a video that is still black or still showing its poster
  // (that swap is what flickered). onFrame is called while it ramps.
  function initDemoVideo(sceneDriven, onFrame) {
    const video = document.getElementById("demo-video");
    const card = document.getElementById("demo-card");
    const toggle = document.getElementById("demo-toggle");
    if (!video || !card || !toggle) return { setInView() {}, shown: () => 0 };

    video.controls = false;
    video.muted = true;
    toggle.hidden = false;

    let userPaused = reducedMotion();
    let inView = false;

    const sync = () => {
      const playing = !video.paused;
      toggle.dataset.state = playing ? "playing" : "paused";
      toggle.setAttribute("aria-label", playing ? "Pause video" : "Play video");
    };
    const play = () => {
      const p = video.play();
      if (p && p.catch) p.catch(sync);
    };
    const update = () => {
      if (inView && !userPaused) play();
      else if (!video.paused) video.pause();
    };

    const RAMP_MS = 350;
    let frameAt = 0;
    const markFrame = () => {
      if (frameAt) return;
      frameAt = performance.now();
      if (onFrame) {
        const pump = () => {
          onFrame();
          if (performance.now() - frameAt < RAMP_MS) requestAnimationFrame(pump);
        };
        requestAnimationFrame(pump);
      }
    };
    const watchFirstFrame = () => {
      if (frameAt) return;
      if (video.requestVideoFrameCallback) video.requestVideoFrameCallback(markFrame);
      else video.addEventListener("timeupdate", () => video.currentTime > 0 && markFrame());
    };
    const shown = () => (frameAt ? clamp01((performance.now() - frameAt) / RAMP_MS) : 0);

    video.addEventListener("play", sync);
    video.addEventListener("pause", sync);
    video.addEventListener("playing", watchFirstFrame, { once: true });
    toggle.addEventListener("click", () => {
      userPaused = !video.paused;
      if (userPaused) video.pause();
      else play();
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
    return { setInView, shown };
  }

  /* ---------- Scroll-driven scenes ---------- */
  // Everything that moves with the scroll position rather than on a timer:
  // the hero receding, the pinned TV scene, the close-ups opening, the Why
  // sentence lighting up word by word, the quotes drifting and the big
  // headlines settling. One rAF-throttled pass per scroll frame.
  function initScenes() {
    const header = document.querySelector(".site-header");
    const hero = document.querySelector(".hero");
    const pin = document.querySelector(".feel");
    const q = (sel) => (pin ? pin.querySelector(sel) : null);
    const sticky = q(".feel-sticky");
    const heading = q(".feel-heading");
    const stage = q(".feel-stage");
    const glow = q(".feel-glow:not(.feel-glow--video)");
    const bezel = q(".feel-bezel");
    const screen = q(".demo-card");
    const poster = q(".feel-poster");
    const video = q("video");
    const toggle = q(".demo-toggle");
    const stand = q(".stand");
    const caption = q(".feel-caption");
    const frames = [...document.querySelectorAll(".closeup-box")].map((frame) => ({
      frame,
      img: frame.querySelector("img"),
      glow: frame.parentElement.querySelector(".closeup-glow"),
    }));
    const drifters = [...document.querySelectorAll("[data-drift]")].map((el) => ({
      el,
      k: Number(el.getAttribute("data-drift")) || 0,
    }));
    const bigs = [...document.querySelectorAll("[data-scrub-big]")];
    const words = splitWords(document.querySelector("[data-words]"));

    const glowVideo = q(".feel-glow--video");
    const demo = initDemoVideo(true, () => schedule());

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

    let raf = 0;
    const tick = () => {
      raf = 0;
      const vh = viewportHeight();
      const vw = document.documentElement.clientWidth;
      const narrow = isNarrow();
      const y = window.scrollY;

      if (header) header.classList.toggle("is-stuck", y > 4);

      if (hero) {
        const p = clamp01(y / (vh * 0.7));
        hero.style.opacity = (1 - p * 0.85).toFixed(3);
        hero.style.transform = p > 0 ? `translateY(${(p * 60).toFixed(1)}px) scale(${(1 - p * 0.06).toFixed(4)})` : "";
        hero.style.filter = p > 0.01 ? `blur(${(p * 6).toFixed(2)}px)` : "";
      }

      if (pin && sticky && stage && bezel && screen) {
        const r = pin.getBoundingClientRect();
        const enter = easeOut3(clamp01((vh - r.top) / vh));
        const p = clamp01(-r.top / Math.max(1, r.height - vh));
        const ss = (a, b) => {
          const t = clamp01((p - a) / (b - a));
          return t * t * (3 - 2 * t);
        };
        // 0.14–0.30: the video fades in over the Home screen.
        // 0.42–0.62: the screen grows to the full window width…
        // 0.84–1.00: …and settles back before the page moves on.
        const fade = ss(0.14, 0.3);
        // Until the video has painted a frame, keep showing the screenshot.
        const vid = fade * demo.shown();
        const zoom = ss(0.42, 0.62) * (1 - ss(0.84, 1));

        const cx = bezel.offsetLeft + screen.offsetLeft + screen.offsetWidth / 2;
        const cy = bezel.offsetTop + screen.offsetTop + screen.offsetHeight / 2;
        const host = sticky.getBoundingClientRect();
        const dx = vw / 2 - (host.left + stage.offsetLeft + cx);
        const dy = vh / 2 - (host.top + stage.offsetTop + cy);
        const full = vw / Math.max(1, screen.offsetWidth);
        const scale = (0.8 + 0.2 * enter) * (1 + (full - 1) * zoom);
        stage.style.transformOrigin = `${cx.toFixed(1)}px ${cy.toFixed(1)}px`;
        stage.style.transform =
          `translate(${(dx * zoom).toFixed(1)}px, ${((1 - enter) * 60 + dy * zoom).toFixed(1)}px) ` +
          `rotateX(${((1 - enter) * 22).toFixed(2)}deg) scale(${scale.toFixed(4)})`;
        stage.style.opacity = (0.3 + 0.7 * enter).toFixed(3);

        if (poster) {
          poster.style.transform = `scale(${(1 + 0.05 * clamp01(p / 0.3)).toFixed(4)})`;
        }
        if (video) {
          video.style.opacity = vid.toFixed(3);
          video.style.transform = `scale(${(1.06 - 0.055 * vid).toFixed(4)})`;
        }
        // The button follows the scroll, not the video, so a viewer whose
        // autoplay was refused (Low Power Mode) can still start it.
        if (toggle) {
          // Counter-scale so the button keeps its size while the TV zooms.
          toggle.style.transform = `scale(${(1 / scale).toFixed(4)})`;
          toggle.style.opacity = fade > 0.9 ? "1" : "0";
          toggle.style.pointerEvents = fade > 0.9 ? "auto" : "none";
        }
        if (stand) stand.style.opacity = (1 - zoom).toFixed(3);
        if (caption) caption.style.opacity = (vid * (1 - zoom)).toFixed(3);
        // The glow behind the TV follows what is on its screen: two blurred
        // copies crossfade, rather than swapping one image's src mid-scroll.
        if (glow) glow.style.opacity = (0.55 * (1 - zoom) * (1 - vid)).toFixed(3);
        if (glowVideo) glowVideo.style.opacity = (0.55 * (1 - zoom) * vid).toFixed(3);
        if (heading) {
          const hv = easeOut3(clamp01((vh - r.top) / (vh * 0.6))) * (1 - ss(0.38, 0.5));
          heading.style.opacity = hv.toFixed(3);
          heading.style.transform = `translateY(${((1 - hv) * (p < 0.3 ? 40 : -24)).toFixed(1)}px)`;
          heading.style.filter = hv < 0.99 ? `blur(${((1 - hv) * 10).toFixed(2)}px)` : "";
        }
        // Start playing as soon as the scene is on screen, well before the
        // fade, so the first frame is decoded by the time it is shown.
        demo.setInView(r.top < vh && r.bottom > 0);
      }

      // Close-ups open from slightly smaller while the picture inside settles
      // from slightly larger, and their glow comes up behind them. Nothing is
      // cropped at rest.
      frames.forEach(({ frame, img, glow }) => {
        const t = easeOut3(clamp01((vh - frame.getBoundingClientRect().top) / (vh * 0.85)));
        const k = 1 - t;
        frame.style.transform = k > 0.001 ? `scale(${(1 - 0.1 * k).toFixed(4)})` : "";
        if (img) img.style.transform = k > 0.001 ? `scale(${(1 + 0.12 * k).toFixed(4)})` : "";
        if (glow) glow.style.opacity = (0.5 * t).toFixed(3);
      });

      // Community quotes drift sideways a little at different rates, so the
      // group reads as layered. Off on phones, where they are one column.
      drifters.forEach(({ el, k }) => {
        if (narrow) {
          el.style.transform = "";
          return;
        }
        const r = el.getBoundingClientRect();
        const t = clamp01((vh - r.top) / (vh + r.height));
        el.style.transform = `translateX(${((0.5 - t) * k * 40).toFixed(1)}px)`;
      });

      // Big headlines start large and faint and settle into place. A scale,
      // not a font-size change, so their line breaks never move.
      bigs.forEach((big) => {
        const t = easeOut3(clamp01((vh - big.getBoundingClientRect().top) / (vh * 0.75)));
        const s0 = narrow ? 0.12 : 0.35;
        big.style.transform = `scale(${(1 + s0 - s0 * t).toFixed(4)})`;
        big.style.opacity = (0.15 + 0.85 * t).toFixed(3);
      });

      if (words.length) {
        const r = words[0].parentElement.getBoundingClientRect();
        const lit = clamp01((vh * 0.8 - r.top) / (r.height + vh * 0.3)) * words.length;
        words.forEach((w, i) => {
          w.style.opacity = (0.2 + 0.8 * clamp01(lit - i)).toFixed(3);
        });
      }
    };

    const schedule = () => {
      if (!raf) raf = requestAnimationFrame(tick);
    };
    const snap = initSnap(pin, viewportHeight);
    window.addEventListener(
      "scroll",
      () => {
        snap.onScroll();
        schedule();
      },
      { passive: true }
    );
    window.addEventListener("resize", schedule);
    // Late-decoding images and fonts shift the layout under the scene.
    window.addEventListener("load", schedule);
    tick();
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

  /* ---------- TV scene snap ---------- */
  // Stopping halfway through the Home→video crossfade or the zoom leaves the
  // TV on a muddled in-between frame. Shortly after scrolling stops (and only
  // with no finger on the screen), glide on to the end of that transition in
  // the direction the reader was going; a small nudge is enough to carry on.
  // The glide is done by hand because CSS scroll snapping cannot target
  // points inside one tall pinned section. Any new input cancels it.
  function initSnap(pin, viewportHeight) {
    const ZONES = [
      [0.13, 0.31],
      [0.41, 0.63],
    ];
    const DURATION_MS = 420;
    let lastY = window.scrollY;
    let dir = 1;
    let touching = false;
    let snapping = false;
    let cancelled = false;
    let timer = 0;

    const glide = () => {
      if (!pin || touching) return;
      const r = pin.getBoundingClientRect();
      const span = Math.max(1, r.height - viewportHeight());
      const p = -r.top / span;
      const zone = ZONES.find(([a, b]) => p > a + 0.003 && p < b - 0.003);
      if (!zone) return;
      const [a, b] = zone;
      const f = (p - a) / (b - a);
      const goOn = dir > 0 ? f > 0.15 : f > 0.85;
      const from = window.scrollY;
      const dist = Math.round((goOn ? b : a) * span + r.top + from) - from;
      const t0 = performance.now();
      const prevBehavior = html.style.scrollBehavior;
      html.style.scrollBehavior = "auto";
      snapping = true;
      cancelled = false;
      const done = () => {
        html.style.scrollBehavior = prevBehavior;
        setTimeout(() => {
          snapping = false;
        }, 60);
      };
      const step = (now) => {
        if (cancelled || touching) return done();
        const k = Math.min(1, (now - t0) / DURATION_MS);
        window.scrollTo(0, from + dist * easeOut3(k));
        if (k < 1) requestAnimationFrame(step);
        else done();
      };
      requestAnimationFrame(step);
    };

    const arm = () => {
      if (snapping) return;
      clearTimeout(timer);
      timer = setTimeout(glide, 160);
    };
    const interrupt = () => {
      clearTimeout(timer);
      if (snapping) cancelled = true;
    };
    const onTouch = (e) => {
      touching = e.type === "touchstart";
      if (touching) interrupt();
      else arm();
    };
    window.addEventListener("touchstart", onTouch, { passive: true });
    window.addEventListener("touchend", onTouch, { passive: true });
    window.addEventListener("touchcancel", onTouch, { passive: true });
    window.addEventListener("wheel", interrupt, { passive: true });
    window.addEventListener("keydown", interrupt);
    window.addEventListener("pointerdown", interrupt);

    return {
      onScroll() {
        const y = window.scrollY;
        if (y !== lastY) {
          dir = y > lastY ? 1 : -1;
          lastY = y;
        }
        arm();
      },
    };
  }

  initReveal();
  initSpotlights();
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
