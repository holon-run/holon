// Theme Management
const initTheme = () => {
    const themeToggle = document.getElementById('theme-toggle');
    const storedTheme = localStorage.getItem('theme');
    const systemPrefersDark = window.matchMedia('(prefers-color-scheme: dark)');

    const setTheme = (theme, persist = true) => {
        document.body.setAttribute('data-theme', theme);
        if (persist) {
            localStorage.setItem('theme', theme);
        }
    };

    // Initial detection
    if (storedTheme) {
        // Respect previously stored user preference
        setTheme(storedTheme);
    } else {
        // Use system preference without persisting as a user choice
        setTheme(systemPrefersDark.matches ? 'dark' : 'light', false);
    }

    // Toggle listener
    themeToggle.addEventListener('click', () => {
        const currentTheme = document.body.getAttribute('data-theme') || 'dark';
        // User toggle should persist the choice
        setTheme(currentTheme === 'dark' ? 'light' : 'dark');
    });

    // Listen for system changes
    systemPrefersDark.addEventListener('change', (e) => {
        if (!localStorage.getItem('theme')) {
            // Follow system changes only when there is no stored user preference
            setTheme(e.matches ? 'dark' : 'light', false);
        }
    });
};

// Update version badge from GitHub
const updateVersionBadge = async () => {
    const badge = document.getElementById('version-badge');
    if (!badge) return;

    try {
        const response = await fetch('https://api.github.com/repos/holon-run/holon/releases/latest');
        if (!response.ok) throw new Error('Network response was not ok');
        const data = await response.json();

        if (data.tag_name) {
            badge.textContent = `${data.tag_name} Public Preview`;
        }
        if (data.html_url) {
            badge.href = data.html_url;
        }
    } catch (error) {
        console.error('Failed to fetch latest release:', error);
        // Fallback to static content already in HTML
    }
};

// Main initialization
document.addEventListener('DOMContentLoaded', () => {
    initTheme();
    updateVersionBadge();
    const tabButtons = document.querySelectorAll('.tab-button');
    const tabContents = document.querySelectorAll('.tab-content');

    tabButtons.forEach(button => {
        button.addEventListener('click', () => {
            const targetTab = button.getAttribute('data-tab');
            if (!targetTab) return;

            const targetContent = document.getElementById(targetTab);
            if (!targetContent) return;

            // Remove active class and update attributes
            tabButtons.forEach(btn => {
                btn.classList.remove('active');
                btn.setAttribute('aria-selected', 'false');
            });
            tabContents.forEach(content => content.classList.remove('active'));

            // Add active class and update attributes
            button.classList.add('active');
            button.setAttribute('aria-selected', 'true');
            targetContent.classList.add('active');
        });
    });

    // Smooth scroll for anchor links
    document.querySelectorAll('a[href^="#"]').forEach(anchor => {
        anchor.addEventListener('click', function (e) {
            const href = this.getAttribute('href');
            if (!href || href === '#') return;

            e.preventDefault();
            const target = document.querySelector(href);
            if (target) {
                const headerOffset = 80;
                const elementPosition = target.getBoundingClientRect().top;
                const offsetPosition = elementPosition + window.pageYOffset - headerOffset;

                window.scrollTo({
                    top: offsetPosition,
                    behavior: 'smooth'
                });
            }
        });
    });

    // Add subtle parallax effect to background glow with requestAnimationFrame
    const backgroundGlow = document.querySelector('.background-glow');
    if (backgroundGlow) {
        let latestScrollY = window.pageYOffset;
        let ticking = false;

        const updateParallax = () => {
            backgroundGlow.style.transform = `translateX(-50%) translateY(${latestScrollY * 0.3}px)`;
            ticking = false;
        };

        window.addEventListener('scroll', () => {
            latestScrollY = window.pageYOffset;
            if (!ticking) {
                ticking = true;
                window.requestAnimationFrame(updateParallax);
            }
        });
    }

    // Add animation on scroll for feature cards
    const observerOptions = {
        threshold: 0.1,
        rootMargin: '0px 0px -50px 0px'
    };

    const observer = new IntersectionObserver((entries) => {
        entries.forEach(entry => {
            if (entry.isIntersecting) {
                entry.target.style.opacity = '1';
                entry.target.style.transform = 'translateY(0)';
            }
        });
    }, observerOptions);

    // Observe feature cards and detail items
    document.querySelectorAll('.feature-card, .detail-item, .step').forEach(el => {
        el.style.opacity = '0';
        el.style.transform = 'translateY(20px)';
        el.style.transition = 'opacity 0.6s ease, transform 0.6s ease';
        observer.observe(el);
    });
});
