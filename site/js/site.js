/* Styrman website: interactivity and motion (self-hosted GSAP + ScrollTrigger, no third-party requests).
   Everything is readable without JS; prefers-reduced-motion keeps content static. */
(function () {
  "use strict";

  var REDUCED = window.matchMedia && window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  var hasGSAP = typeof window.gsap !== "undefined";
  var hasST = hasGSAP && typeof window.ScrollTrigger !== "undefined";
  if (hasST) window.gsap.registerPlugin(window.ScrollTrigger);
  document.documentElement.classList.add("has-js");
  if (hasGSAP && !REDUCED) document.documentElement.classList.add("has-gsap");

  function onView(el, fn, margin) {
    if (!el) return;
    if (!("IntersectionObserver" in window)) { fn(); return; }
    var io = new IntersectionObserver(function (entries) {
      entries.forEach(function (e) { if (e.isIntersecting) { io.disconnect(); fn(); } });
    }, { rootMargin: margin || "0px 0px -12% 0px" });
    io.observe(el);
  }

  /* 1. Header: frosted once you scroll */
  var header = document.querySelector(".site-header");
  function onScroll() { header && header.classList.toggle("scrolled", window.scrollY > 12); }
  window.addEventListener("scroll", onScroll, { passive: true });
  window.addEventListener("load", onScroll);
  window.addEventListener("hashchange", onScroll);
  onScroll();

  /* 2. Hero: the agent asks in the terminal, the phone buzzes, you allow it from the phone */
  (function heroStory() {
    var term = document.querySelector("[data-typed]");
    if (!term) return;
    var steps = Array.prototype.slice.call(term.querySelectorAll("[data-step]"));
    var toast = document.querySelector(".toast");
    function show(n) {
      steps.forEach(function (el) { el.classList.toggle("on", +el.dataset.step <= n); });
      if (toast) { toast.classList.toggle("on", n === 4); toast.classList.toggle("gone", n > 4); }
    }
    if (REDUCED) { show(6); if (toast) toast.classList.remove("on"); return; }
    var timings = [500, 900, 1000, 900, 900, 2600, 900];
    var n = -1, timer;
    function next() {
      n += 1;
      if (n > 6) { timer = setTimeout(function () { n = -1; show(-1); timer = setTimeout(next, 700); }, 4200); return; }
      show(n);
      timer = setTimeout(next, timings[n + 1] || 900);
    }
    show(-1);
    timer = setTimeout(next, 700);
    document.addEventListener("visibilitychange", function () {
      if (document.hidden) clearTimeout(timer); else { clearTimeout(timer); timer = setTimeout(next, 600); }
    });
  })();

  /* 3. Status chips change like the real list does */
  (function chips() {
    var list = document.querySelectorAll(".chip[data-cycle]");
    list.forEach(function (c) { c.dataset.t = c.dataset.tone.split("|")[0]; });
    if (REDUCED || !list.length) return;
    var i = 0;
    onView(document.querySelector(".status-list"), function () {
      setInterval(function () {
        i += 1;
        list.forEach(function (c, k) {
          var labels = c.dataset.cycle.split("|"), tones = c.dataset.tone.split("|");
          var j = (i + (k === 2 ? 0 : 0)) % labels.length;
          c.textContent = labels[j];
          c.dataset.t = tones[j];
        });
      }, 2400);
    });
  })();

  /* 4. Voice: the play button reads the reply aloud (with the browser's own voice) */
  (function voice() {
    var box = document.querySelector(".voice");
    var btn = box && box.querySelector(".play");
    if (!btn) return;
    var text = "Looks good. Backoff is capped at 30 seconds and jittered.";
    var playing = false, fallback;
    function stop() {
      playing = false; box.classList.remove("playing"); clearTimeout(fallback);
      btn.setAttribute("aria-label", "Play a sample reply");
    }
    btn.addEventListener("click", function () {
      if (playing) { if (window.speechSynthesis) speechSynthesis.cancel(); stop(); return; }
      playing = true; box.classList.add("playing");
      btn.setAttribute("aria-label", "Stop");
      if (window.speechSynthesis && window.SpeechSynthesisUtterance) {
        var u = new SpeechSynthesisUtterance(text);
        u.lang = "en-US"; u.rate = 1.02;
        u.onend = stop; u.onerror = stop;
        speechSynthesis.cancel(); speechSynthesis.speak(u);
        fallback = setTimeout(stop, 9000);
      } else {
        fallback = setTimeout(stop, 3500);
      }
    });
  })();

  /* 5. Usage rings fill in, with counters */
  (function rings() {
    var box = document.querySelector(".rings");
    if (!box) return;
    onView(box, function () {
      box.querySelectorAll(".arc").forEach(function (a) { a.style.setProperty("--p", (+a.dataset.arc / 100).toString()); });
      if (REDUCED) return;
      box.querySelectorAll("[data-count-to]").forEach(function (el) {
        var to = +el.dataset.countTo, suffix = el.dataset.countSuffix || "", t0 = performance.now();
        (function tick(t) {
          var k = Math.min(1, (t - t0) / 1200), e = 1 - Math.pow(1 - k, 3);
          el.textContent = Math.round(to * e) + suffix;
          if (k < 1) requestAnimationFrame(tick);
        })(t0);
      });
    });
  })();

  /* 6. Copy buttons */
  document.querySelectorAll(".copy[data-copy]").forEach(function (b) {
    b.addEventListener("click", function () {
      var done = function () { b.textContent = "Copied"; b.classList.add("done"); setTimeout(function () { b.textContent = "Copy"; b.classList.remove("done"); }, 1600); };
      if (navigator.clipboard) navigator.clipboard.writeText(b.dataset.copy).then(done, function () {});
    });
  });

  /* 7. GSAP choreography */
  if (!(hasGSAP && hasST && !REDUCED)) return;
  var gsap = window.gsap;

  // Hero entrance: movement only, so the hero is readable from the first frame.
  gsap.timeline({ defaults: { ease: "power3.out" } })
    .from(".hero .pill", { y: 16, duration: 0.55 })
    .from(".hero h1", { y: 26, duration: 0.7 }, "-=0.35")
    .from(".hero .lead", { y: 18, duration: 0.6 }, "-=0.45")
    .from(".hero .cta-row .btn", { y: 16, stagger: 0.08, duration: 0.55 }, "-=0.4")
    .from(".hero .cta-note, .hero .hero-meta", { y: 12, stagger: 0.08, duration: 0.5 }, "-=0.35")
    .from(".hero-art .term-hero", { y: 40, scale: 0.96, duration: 0.9, ease: "power2.out" }, 0.15)
    .from(".hero-art .phone-hero", { y: 60, scale: 0.94, duration: 0.95, ease: "power2.out" }, 0.3);

  // Depth: the phone and terminal follow the pointer a little.
  var hero = document.querySelector(".hero");
  var term = document.querySelector(".term-hero");
  var phone = document.querySelector(".phone-hero");
  if (hero && term && phone && window.matchMedia("(min-width: 62rem) and (pointer: fine)").matches) {
    var tX = gsap.quickTo(term, "x", { duration: 0.6, ease: "power2.out" });
    var tY = gsap.quickTo(term, "y", { duration: 0.6, ease: "power2.out" });
    var pX = gsap.quickTo(phone, "x", { duration: 0.5, ease: "power2.out" });
    var pY = gsap.quickTo(phone, "y", { duration: 0.5, ease: "power2.out" });
    hero.addEventListener("pointermove", function (e) {
      var r = hero.getBoundingClientRect();
      var nx = (e.clientX - r.left) / r.width - 0.5, ny = (e.clientY - r.top) / r.height - 0.5;
      tX(nx * -14); tY(ny * -10); pX(nx * 22); pY(ny * 16);
    }, { passive: true });
    hero.addEventListener("pointerleave", function () { tX(0); tY(0); pX(0); pY(0); });
  }
  if (term && phone) {
    gsap.to(term, { yPercent: -6, ease: "none", scrollTrigger: { trigger: ".hero", start: "top top", end: "bottom top", scrub: 0.6 } });
    gsap.to(phone, { yPercent: -14, ease: "none", scrollTrigger: { trigger: ".hero", start: "top top", end: "bottom top", scrub: 0.6 } });
  }

  gsap.from(".works li", { y: 10, opacity: 0, stagger: 0.05, duration: 0.45, ease: "power2.out", scrollTrigger: { trigger: ".works", start: "top 92%", once: true } });

  // Stage: the terminal tilts back as the phone takes over.
  var stage = document.querySelector(".stage");
  if (stage) {
    gsap.fromTo(".term-tabs", { rotateY: 0, rotateX: 0, scale: 1 }, {
      rotateY: 10, rotateX: 4, scale: 0.94, transformPerspective: 1100, ease: "none",
      scrollTrigger: { trigger: stage, start: "top 75%", end: "center 40%", scrub: 0.7 }
    });
    gsap.from(".stage .arrow", { scale: 0.6, opacity: 0, duration: 0.55, ease: "back.out(1.7)", scrollTrigger: { trigger: stage, start: "top 70%", once: true } });
    gsap.from(".stage-phone .phone", { y: 50, opacity: 0, scale: 0.94, duration: 0.8, ease: "power3.out", scrollTrigger: { trigger: stage, start: "top 72%", once: true } });
    gsap.from(".tab", { y: -8, opacity: 0, stagger: 0.12, duration: 0.4, scrollTrigger: { trigger: stage, start: "top 78%", once: true } });
  }

  // The consult cell's phone slides up into place.
  var consult = document.querySelector(".c-consult .phone");
  if (consult) gsap.from(consult, { y: 80, duration: 1, ease: "power3.out", scrollTrigger: { trigger: consult, start: "top 92%", once: true } });

  gsap.utils.toArray(".reveal").forEach(function (el) {
    gsap.from(el, { y: 26, opacity: 0, duration: 0.7, ease: "power2.out", clearProps: "transform", scrollTrigger: { trigger: el, start: "top 88%", once: true } });
  });
})();
