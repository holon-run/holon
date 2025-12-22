document.addEventListener('DOMContentLoaded', () => {
    // 1. Smooth Scroll for Anchor Links
    document.querySelectorAll('a[href^="#"]').forEach(anchor => {
        anchor.addEventListener('click', function (e) {
            e.preventDefault();
            const target = document.querySelector(this.getAttribute('href'));
            if (target) {
                target.scrollIntoView({
                    behavior: 'smooth',
                    block: 'start'
                });
            }
        });
    });

    // 2. Intersection Observer for Fade-in Animations
    const observerOptions = {
        threshold: 0.1,
        rootMargin: '0px 0px -50px 0px'
    };

    const observer = new IntersectionObserver((entries) => {
        entries.forEach(entry => {
            if (entry.isIntersecting) {
                entry.target.classList.add('visible');
                observer.unobserve(entry.target); // Only animate once
            }
        });
    }, observerOptions);

    // Elements to animate
    const animatedElements = document.querySelectorAll('.feature-card, .detail-item, .step, .comparison-box, .section-header');

    // Add initial styles for animation
    const style = document.createElement('style');
    style.innerHTML = `
        .feature-card, .detail-item, .step, .comparison-box, .section-header {
            opacity: 0;
            transform: translateY(20px);
            transition: opacity 0.6s ease-out, transform 0.6s ease-out;
        }
        .visible {
            opacity: 1;
            transform: translateY(0);
        }
        /* Staggered delay for grid items */
        .feature-card:nth-child(2) { transition-delay: 0.1s; }
        .feature-card:nth-child(3) { transition-delay: 0.2s; }
    `;
    document.head.appendChild(style);

    animatedElements.forEach(el => observer.observe(el));

    // 3. Typewriter Effect for Terminal
    const terminalLines = document.querySelectorAll('.terminal-body .code-line');

    // Hide all lines initially except the first prompt
    terminalLines.forEach((line, index) => {
        if (index > 0) {
            line.style.opacity = '0';
            line.style.display = 'block'; // Ensure layout space is taken or keep hidden? 
            // Better to hide opacity to keep layout stable if heights differ, 
            // but for terminal list, valid to just append.
            // Let's use opacity and transform for a smooth "appearance"
            line.style.transform = 'translateY(5px)';
            line.style.transition = 'opacity 0.3s, transform 0.3s';
        }
    });

    // Sequence the appearance
    let delay = 1000;
    terminalLines.forEach((line, index) => {
        if (index === 0) return; // Skip first line (already visible)

        let currentDelay = 0;

        // Vary delay based on content "simulating work"
        if (line.textContent.includes("Initializing")) currentDelay = 800;
        else if (line.textContent.includes("Snapshotting")) currentDelay = 1200;
        else if (line.textContent.includes("Running agent")) currentDelay = 2500;
        else currentDelay = 600;

        delay += currentDelay;

        setTimeout(() => {
            line.style.opacity = '1';
            line.style.transform = 'translateY(0)';
        }, delay);
    });
});
