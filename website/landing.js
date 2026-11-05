// Prepare the first install method and the entrances before the landing page's
// first layout. Without JavaScript, every method remains visible, commands can
// be selected, and the page is complete and still.
(function () {
  var root = document.documentElement;
  root.classList.add('landing-enhancing');
  // Entrances are prepared here, before the first paint, so content never
  // flashes before it rises. Anything that stops motion() shows it again.
  var capable = !!(window.matchMedia && Element.prototype.animate && 'IntersectionObserver' in window);
  if (capable && !window.matchMedia('(prefers-reduced-motion: reduce)').matches) root.setAttribute('data-entrances-ready', '');
  function enhance() {
    var installer = document.getElementById('install');
    var tablist = document.querySelector('.install-tabs');
    var panels = Array.prototype.slice.call(document.querySelectorAll('.install-panel'));
    var tabs = Array.prototype.slice.call(document.querySelectorAll('.install-tabs [role="tab"]'));
    var copies = Array.prototype.slice.call(document.querySelectorAll('button[data-copy]'));
    var saved = [];
    try {
      saved = panels.concat(tabs).concat(copies).concat([installer, tablist]).filter(Boolean).map(function (element) {
        return { element: element, attributes: ['hidden', 'role', 'aria-labelledby', 'aria-selected', 'tabindex', 'class'].map(function (name) {
          return { name: name, value: element.getAttribute(name) };
        }) };
      });
      (function () {
        var status = document.getElementById('copy-status');

        // Copy buttons. Without clipboard access, select the command so it can be
        // copied by hand, and say so.
        document.querySelectorAll('button[data-copy]').forEach(function (button) {
          button.hidden = false;
          button.addEventListener('click', function () {
            var code = document.getElementById(button.dataset.copy);
            var select = function () {
              var range = document.createRange();
              range.selectNodeContents(code);
              var selection = window.getSelection();
              selection.removeAllRanges();
              selection.addRange(range);
              code.focus();
              status.textContent = 'Command selected; press Ctrl+C, or Command+C on a Mac, to copy it';
            };
            if (!navigator.clipboard || !window.isSecureContext) { select(); return; }
            navigator.clipboard.writeText(code.textContent).then(function () {
              button.textContent = 'Copied';
              status.textContent = 'Copied to the clipboard';
              setTimeout(function () { button.textContent = 'Copy'; }, 1600);
            }, select);
          });
        });

        // Install tabs: arrow keys, Home and End move between them.
        var tabs = Array.prototype.slice.call(document.querySelectorAll('.install-tabs [role="tab"]'));
        function choose(tab, focus) {
          tabs.forEach(function (other) {
            var chosen = other === tab;
            other.setAttribute('aria-selected', chosen ? 'true' : 'false');
            other.tabIndex = chosen ? 0 : -1;
            var panel = document.getElementById(other.getAttribute('aria-controls'));
            panel.setAttribute('role', 'tabpanel');
            panel.setAttribute('aria-labelledby', other.id);
            panel.hidden = !chosen;
          });
          if (focus) tab.focus();
        }
        tabs.forEach(function (tab, index) {
          tab.addEventListener('click', function () { choose(tab, false); });
          tab.addEventListener('keydown', function (event) {
            var next = null;
            if (event.key === 'ArrowRight') next = tabs[(index + 1) % tabs.length];
            else if (event.key === 'ArrowLeft') next = tabs[(index - 1 + tabs.length) % tabs.length];
            else if (event.key === 'Home') next = tabs[0];
            else if (event.key === 'End') next = tabs[tabs.length - 1];
            if (next) { event.preventDefault(); choose(next, true); }
          });
        });
        if (tabs.length) {
          // Pending CSS shows #panel-script, the panel controlled by tabs[0].
          choose(tabs[0], false);
          document.querySelector('.install-tabs').hidden = false;
          document.getElementById('install').classList.add('enhanced');
        }
      })();

    } catch (error) {
      // Restore the fallback even if a panel changed before setup failed.
      saved.forEach(function (state) {
        state.attributes.forEach(function (attribute) {
          if (attribute.value === null) state.element.removeAttribute(attribute.name);
          else state.element.setAttribute(attribute.name, attribute.value);
        });
      });
      console.error(error);
    } finally {
      root.classList.remove('landing-enhancing');
    }
  }
  // Motion. The HTML is the complete, static result: without JavaScript, with
  // reduced motion or without these APIs, nothing moves and nothing is hidden.
  function motion() {
    var reduced = window.matchMedia('(prefers-reduced-motion: reduce)');
    var revealTargets = Array.prototype.slice.call(document.querySelectorAll('[data-reveal]'));
    var words = Array.prototype.slice.call(document.querySelectorAll('[data-word]'));
    var entrances = new Map();
    function settle(element) {
      element.setAttribute('data-revealed', '');
      var animation = entrances.get(element);
      if (animation) animation.finish();
    }
    function settleAll() { words.concat(revealTargets).forEach(settle); }

    // Entrances: the headline rises once, then each block fades up as it enters view.
    var terminal = document.querySelector('.terminal');
    if (!root.hasAttribute('data-entrances-ready')) {
      settleAll();
    } else {
      words.forEach(function (word, index) {
        var animation = word.animate(
          [{ transform: 'translateY(105%)' }, { transform: 'translateY(0)' }],
          { duration: 800, delay: 120 + index * 80, easing: 'cubic-bezier(.2,.7,.2,1)', fill: 'backwards' });
        entrances.set(word, animation);
        animation.onfinish = function () { entrances.delete(word); };
        word.setAttribute('data-revealed', '');
      });
      var observer = new IntersectionObserver(function (entries) {
        var delay = 0;
        entries.forEach(function (entry) {
          if (!entry.isIntersecting) return;
          var element = entry.target;
          observer.unobserve(element);
          var animation = element.animate(
            [{ opacity: 0, transform: 'translateY(32px)' }, { opacity: 1, transform: 'translateY(0)' }],
            { duration: 1100, delay: delay, easing: 'cubic-bezier(.25,.1,.25,1)', fill: 'backwards' });
          entrances.set(element, animation);
          animation.onfinish = function () { entrances.delete(element); };
          delay = Math.min(delay + 120, 360);
          element.setAttribute('data-revealed', '');
          if (element === terminal) play(element, delay);
        });
      }, { rootMargin: '0px 0px -48px 0px', threshold: 0.05 });
      revealTargets.forEach(function (element) { observer.observe(element); });
      // Keyboard and find-in-page users never wait for an entrance.
      document.addEventListener('focusin', function (event) {
        revealTargets.forEach(function (element) {
          if (!element.hasAttribute('data-revealed') && element.contains(event.target)) {
            observer.unobserve(element);
            settle(element);
          }
        });
      });
      window.addEventListener('beforeprint', settleAll);
    }

    // The terminal prints its output line by line, once, then the cursor blinks.
    function play(figure, delay) {
      var code = figure.querySelector('code');
      var lines = Math.max(1, code.textContent.split('\n').length - 1);
      var duration = Math.min(2600, lines * 110);
      var printing = code.animate(
        [{ clipPath: 'inset(0 0 100% 0)' }, { clipPath: 'inset(0 0 0 0)' }],
        { duration: duration, delay: delay + 300, easing: 'steps(' + lines + ', end)', fill: 'backwards' });
      entrances.set(code, printing);
      printing.onfinish = function () {
        entrances.delete(code);
        var cursor = code.querySelector('.cursor');
        if (cursor && !reduced.matches) {
          cursor.animate([{ opacity: 1 }, { opacity: 0 }], { duration: 1100, iterations: 3, easing: 'steps(2, jump-none)' });
        }
      };
    }

    // Decorative loops: the hero glow and capacity trace, the installer's aura
    // and the Data Phase packets. They run only while visible, stop in hidden
    // tabs, and one page-wide control pauses all of them.
    var button = document.getElementById('motion-toggle');
    var requested = true;
    try { requested = window.localStorage.getItem('udpstp-motion') !== 'paused'; } catch (error) { /* storage blocked */ }
    var groups = [];
    function group(element, animations, pausesOnHover) {
      if (!element || !animations.length) return;
      animations.forEach(function (animation) { animation.pause(); });
      groups.push({ element: element, animations: animations, visible: false, hovered: false, pausesOnHover: pausesOnHover });
    }
    var glow = document.querySelector('[data-motion="breathe"]');
    if (glow) {
      group(glow, [glow.animate(
        [{ opacity: 0.55, transform: 'scale(0.96)' }, { opacity: 1, transform: 'scale(1.06)' }, { opacity: 0.55, transform: 'scale(0.96)' }],
        { duration: 8000, iterations: Infinity, easing: 'ease-in-out' })], false);
    }
    var trace = document.querySelector('[data-motion="trace"]');
    if (trace) {
      // A pulse of light follows the capacity search to its plateau.
      group(trace.closest('.hero-scene'), [trace.animate(
        [{ strokeDashoffset: 1.12 }, { strokeDashoffset: 0, offset: 0.7 }, { strokeDashoffset: -0.12 }],
        { duration: 9000, iterations: Infinity, easing: 'ease-in-out' })], false);
    }
    var installer = document.querySelector('[data-aura]');
    if (installer) {
      try {
        group(installer, [installer.animate(
          [{ opacity: 0.6, transform: 'scale(0.98)' }, { opacity: 1, transform: 'scale(1.04)' }, { opacity: 0.6, transform: 'scale(0.98)' }],
          { duration: 6000, iterations: Infinity, easing: 'ease-in-out', pseudoElement: '::before' })], false);
      } catch (error) { /* no pseudo-element animation: the glow stays still */ }
    }
    // The Data Phase of a Downstream Test: Load PDUs stream from the Server to
    // the Client, a Status PDU returns every Trial Interval, and the Server's
    // algorithm answers it.
    var illustration = document.getElementById('data-phase-illustration');
    if (illustration) {
      var TRIAL = 3000;
      var loads = Array.prototype.slice.call(illustration.querySelectorAll('[data-packet="load"]'));
      var animations = loads.map(function (packet, index) {
        return packet.animate(
          [{ left: '100%', opacity: 0 }, { opacity: 1, offset: 0.1 }, { opacity: 1, offset: 0.9 }, { left: '0%', opacity: 0 }],
          { duration: 2400, iterations: Infinity, iterationStart: index / loads.length, easing: 'linear' });
      });
      var status = illustration.querySelector('[data-packet="status"]');
      if (status) {
        animations.push(status.animate(
          [{ left: '0%', opacity: 0 }, { opacity: 1, offset: 0.04 }, { left: '100%', opacity: 1, offset: 0.4 }, { left: '100%', opacity: 0, offset: 0.46 }, { left: '100%', opacity: 0 }],
          { duration: TRIAL, iterations: Infinity, easing: 'linear' }));
      }
      var algorithm = illustration.querySelector('[data-packet="algorithm"]');
      if (algorithm) {
        animations.push(algorithm.animate(
          [{ boxShadow: '0 0 0 0 var(--packet-pulse)' }, { boxShadow: '0 0 0 0 var(--packet-pulse)', offset: 0.4 },
            { boxShadow: '0 0 0 8px var(--packet-pulse)', offset: 0.5 }, { boxShadow: '0 0 0 0 var(--packet-pulse)', offset: 0.7 },
            { boxShadow: '0 0 0 0 var(--packet-pulse)' }],
          { duration: TRIAL, iterations: Infinity, easing: 'ease-out' }));
      }
      group(illustration, animations, true);
    }

    function hold(animation) {
      var time = animation.currentTime;
      animation.pause();
      if (time !== null) animation.currentTime = time;
    }
    function update() {
      var still = reduced.matches;
      var paused = !requested || still;
      root.toggleAttribute('data-motion-paused', paused);
      root.toggleAttribute('data-motion-running', !paused);
      groups.forEach(function (entry) {
        var running = !paused && !document.hidden && entry.visible && !(entry.pausesOnHover && entry.hovered);
        entry.animations.forEach(function (animation) {
          if (still) animation.cancel();
          else if (running) animation.play();
          else hold(animation);
        });
      });
      if (button) {
        button.textContent = still ? 'Motion off' : requested ? 'Pause motion' : 'Play motion';
        button.disabled = still;
      }
    }
    if (groups.length) {
      var visibility = new IntersectionObserver(function (entries) {
        entries.forEach(function (entry) {
          groups.forEach(function (candidate) {
            if (candidate.element === entry.target) candidate.visible = entry.isIntersecting;
          });
        });
        update();
      }, { threshold: 0.1 });
      groups.forEach(function (entry) {
        visibility.observe(entry.element);
        if (!entry.pausesOnHover) return;
        // Hold the illustration still while someone reads or points at it.
        var on = function () { entry.hovered = true; update(); };
        var off = function () { entry.hovered = entry.element.matches(':hover') || entry.element.contains(document.activeElement); update(); };
        entry.element.addEventListener('mouseenter', on);
        entry.element.addEventListener('mouseleave', off);
        entry.element.addEventListener('focusin', on);
        entry.element.addEventListener('focusout', function () { setTimeout(off, 0); });
      });
      if (button) {
        button.addEventListener('click', function () {
          requested = !requested;
          try { window.localStorage.setItem('udpstp-motion', requested ? 'running' : 'paused'); } catch (error) { /* storage blocked */ }
          update();
        });
        button.hidden = false;
      }
      document.addEventListener('visibilitychange', update);
      if (reduced.addEventListener) reduced.addEventListener('change', update);
    }
    update();
  }
  function ready() {
    enhance();
    // Motion is independent of the installer setup and its fallback recovery.
    if (!capable) return;
    try { motion(); } catch (error) {
      root.removeAttribute('data-entrances-ready');
      console.error(error);
    }
  }
  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', ready, { once: true });
  else ready();
})();
