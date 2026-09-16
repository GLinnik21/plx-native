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
  function initDemoVideo() {
    const video = document.getElementById("demo-video");
    const card = document.getElementById("demo-card");
    if (!video || !card) return;

    if ("IntersectionObserver" in window) {
      const observer = new IntersectionObserver(
        (entries) => {
          entries.forEach((entry) => {
            if (!entry.isIntersecting && !video.paused) video.pause();
          });
        },
        { threshold: 0 }
      );
      observer.observe(card);
    }
  }

  initReveal();
  initDemoVideo();
})();
