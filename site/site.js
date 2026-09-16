(() => {
  const reducedMotion = () =>
    window.matchMedia && window.matchMedia("(prefers-reduced-motion: reduce)").matches;

  /* ---------- Section reveal ---------- */
  function initReveal() {
    if (reducedMotion() || !("IntersectionObserver" in window)) return;
    const root = document.querySelector(".page-root");
    if (!root) return;
    const targets = [...root.querySelectorAll(":scope > section, :scope > footer")].slice(1);
    if (!targets.length) return;

    targets.forEach((el) => el.classList.add("reveal-pending"));

    const reveal = (el) => {
      el.classList.remove("reveal-pending");
      el.classList.add("reveal-in");
    };

    const observer = new IntersectionObserver(
      (entries, obs) => {
        entries.forEach((entry) => {
          if (!entry.isIntersecting) return;
          reveal(entry.target);
          obs.unobserve(entry.target);
        });
      },
      { rootMargin: "0px 0px -12% 0px", threshold: 0.06 }
    );
    targets.forEach((el) => observer.observe(el));

    // A short trailing element (the footer) can sit inside the shrunk root
    // margin without ever crossing the intersection threshold once the page
    // can scroll no further. Once the viewport reaches the bottom of the
    // document, force-reveal anything still pending so it never gets stuck.
    const revealAtBottom = () => {
      const atBottom =
        window.innerHeight + window.scrollY >= document.documentElement.scrollHeight - 2;
      if (!atBottom) return;
      targets.forEach((el) => {
        if (!el.classList.contains("reveal-pending")) return;
        reveal(el);
        observer.unobserve(el);
      });
    };
    window.addEventListener("scroll", revealAtBottom, { passive: true });
    window.addEventListener("resize", revealAtBottom);
    revealAtBottom();
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
