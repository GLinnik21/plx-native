(() => {
  const image = document.querySelector("#showcase-image");
  const frame = document.querySelector(".showcase-shot-frame");
  const tabs = [...document.querySelectorAll("[data-shot]")];
  const shots = {
    library: { src: "screenshots/library.jpg", alt: "PlxNative Browse screen" },
    search: { src: "screenshots/search.jpg", alt: "PlxNative Search screen" },
    player: { src: "screenshots/player.jpg", alt: "PlxNative Playback screen" },
  };

  const setShot = (name) => {
    const shot = shots[name];
    if (!shot || !image || !frame) return;
    tabs.forEach((tab) => {
      const active = tab.dataset.shot === name;
      tab.classList.toggle("is-active", active);
      tab.setAttribute("aria-selected", String(active));
    });
    frame.classList.add("is-changing");
    const next = new Image();
    next.onload = () => {
      image.src = shot.src;
      image.alt = shot.alt;
      requestAnimationFrame(() => frame.classList.remove("is-changing"));
    };
    next.src = shot.src;
  };

  tabs.forEach((tab) => {
    tab.addEventListener("click", () => setShot(tab.dataset.shot));
    tab.addEventListener("keydown", (event) => {
      if (event.key !== "ArrowRight" && event.key !== "ArrowLeft") return;
      event.preventDefault();
      const index = tabs.indexOf(tab);
      const nextIndex = event.key === "ArrowRight"
        ? (index + 1) % tabs.length
        : (index - 1 + tabs.length) % tabs.length;
      tabs[nextIndex].focus();
      setShot(tabs[nextIndex].dataset.shot);
    });
  });

  if ("IntersectionObserver" in window && !window.matchMedia("(prefers-reduced-motion: reduce)").matches) {
    const sections = document.querySelectorAll(".split-section, .showcase-section, .built-callout, .quote-section, .install-section, .notes-section, .site-footer");
    const observer = new IntersectionObserver((entries, instance) => {
      entries.forEach((entry) => {
        if (!entry.isIntersecting) return;
        entry.target.style.animationPlayState = "running";
        instance.unobserve(entry.target);
      });
    }, { rootMargin: "0px 0px -12% 0px", threshold: 0.06 });
    sections.forEach((section) => {
      section.style.animationPlayState = "paused";
      observer.observe(section);
    });
  }
})();
