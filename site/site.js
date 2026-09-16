(() => {
  const reducedMotion = () =>
    window.matchMedia && window.matchMedia("(prefers-reduced-motion: reduce)").matches;

  /* ---------- Scroll reveal ---------- */
  // Reveals [data-reveal] elements as they enter the viewport, staggering the
  // ones that enter together. An element is reset only once it is entirely
  // BELOW the viewport (the reader scrolled back up past it): the hidden state
  // moves it further down, so the reset can never pull it back into view and
  // flicker, and scrolling down again replays the animation.
  function initReveal() {
    const html = document.documentElement;
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
      { rootMargin: "0px 0px -10% 0px", threshold: 0 }
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
  }

  /* ---------- Demo video ---------- */
  // Plays by itself while at least half of it is on screen, like a product
  // page loop. The markup ships native controls so the video still works
  // without JS; with JS they are replaced by a single pause/play button.
  // Reduced motion starts paused, and a viewer's pause is never overridden.
  function initDemoVideo() {
    const video = document.getElementById("demo-video");
    const card = document.getElementById("demo-card");
    const toggle = document.getElementById("demo-toggle");
    if (!video || !card || !toggle) return;

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

    video.addEventListener("play", sync);
    video.addEventListener("pause", sync);
    toggle.addEventListener("click", () => {
      userPaused = !video.paused;
      if (userPaused) video.pause();
      else play();
    });

    if ("IntersectionObserver" in window) {
      new IntersectionObserver(
        (entries) => {
          entries.forEach((entry) => {
            inView = entry.isIntersecting;
          });
          update();
        },
        { threshold: 0.5 }
      ).observe(card);
    } else {
      inView = true;
      update();
    }
    sync();
  }

  initReveal();
  initDemoVideo();
})();
