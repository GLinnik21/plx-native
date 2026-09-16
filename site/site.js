(() => {
  const EASING = "cubic-bezier(.16,1,.3,1)";

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

  /* ---------- Showcase tabs ---------- */
  function initShowcase() {
    const tabs = [...document.querySelectorAll(".showcase-tab")];
    const image = document.getElementById("showcase-image");
    const wrap = document.getElementById("showcase-shot-wrap");
    const mobileBlurb = document.getElementById("showcase-mobile-blurb");
    const panel = document.getElementById("showcase-panel");
    if (!tabs.length || !image || !wrap) return;

    const loaded = new Set();
    let activeIndex = tabs.findIndex((t) => t.classList.contains("is-active"));
    if (activeIndex < 0) activeIndex = 0;

    tabs.forEach((tab) => {
      const src = tab.dataset.img;
      const probe = new Image();
      probe.onload = () => loaded.add(src);
      probe.src = src;
    });
    image.addEventListener("load", () => {
      loaded.add(image.getAttribute("src"));
    });

    function simpleFade(src, alt, duration) {
      image.src = src;
      image.alt = alt;
      if (image.animate) {
        image.animate([{ opacity: 0 }, { opacity: 1 }], { duration, easing: "linear" });
      }
    }

    function ghostCrossSlide(direction, src, alt) {
      const ghost = image.cloneNode(true);
      ghost.removeAttribute("id");
      ghost.classList.add("showcase-ghost");
      wrap.appendChild(ghost);
      if (ghost.animate) {
        const ghostAnim = ghost.animate(
          [
            { opacity: 1, transform: "translateX(0) scale(1)" },
            { opacity: 0, transform: `translateX(${-direction * 3}%) scale(.995)` },
          ],
          { duration: 300, easing: EASING, fill: "forwards" }
        );
        ghostAnim.onfinish = () => ghost.remove();
      } else {
        ghost.remove();
      }

      image.src = src;
      image.alt = alt;
      if (image.animate) {
        image.animate(
          [
            { opacity: 0, transform: `translateX(${direction * 5}%) scale(1.01)` },
            { opacity: 1, transform: "none" },
          ],
          { duration: 420, easing: EASING }
        );
      }
    }

    function animateBlurb(newTab, reduced) {
      const activeBlurbEl = newTab.querySelector(".tab-blurb");
      const targets = [activeBlurbEl, mobileBlurb].filter(Boolean);
      if (reduced) return;
      targets.forEach((el) => {
        if (el.animate) {
          el.animate(
            [
              { opacity: 0, transform: "translateY(6px)" },
              { opacity: 1, transform: "none" },
            ],
            { duration: 340, easing: EASING }
          );
        }
      });
    }

    function goToTab(newIndex) {
      if (newIndex === activeIndex) return;
      const prevTab = tabs[activeIndex];
      const newTab = tabs[newIndex];
      const direction = newIndex > activeIndex ? 1 : -1;
      activeIndex = newIndex;

      tabs.forEach((tab, i) => {
        const active = i === newIndex;
        tab.classList.toggle("is-active", active);
        tab.setAttribute("aria-selected", String(active));
        tab.tabIndex = active ? 0 : -1;
      });

      if (mobileBlurb) mobileBlurb.textContent = newTab.dataset.blurb;
      if (panel) panel.setAttribute("aria-labelledby", newTab.id);

      const prevSrc = prevTab.dataset.img;
      const newSrc = newTab.dataset.img;
      const outgoingLoaded = loaded.has(prevSrc);
      const incomingLoaded = loaded.has(newSrc);
      const reduced = reducedMotion();

      if (!outgoingLoaded || !incomingLoaded) {
        simpleFade(newSrc, newTab.dataset.alt, 160);
      } else if (reduced) {
        simpleFade(newSrc, newTab.dataset.alt, 200);
      } else {
        ghostCrossSlide(direction, newSrc, newTab.dataset.alt);
      }

      animateBlurb(newTab, reduced);
    }

    tabs.forEach((tab, i) => {
      tab.tabIndex = i === activeIndex ? 0 : -1;
      tab.addEventListener("click", () => goToTab(i));
      tab.addEventListener("keydown", (event) => {
        if (event.key !== "ArrowRight" && event.key !== "ArrowLeft") return;
        event.preventDefault();
        const nextIndex =
          event.key === "ArrowRight" ? (i + 1) % tabs.length : (i - 1 + tabs.length) % tabs.length;
        tabs[nextIndex].focus();
        goToTab(nextIndex);
      });
    });
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
  initShowcase();
  initDemoVideo();
})();
