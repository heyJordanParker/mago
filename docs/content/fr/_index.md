+++
title = "Mago"
description = "La chaîne d'outils PHP oxydée. Un analyseur statique, un linter et un formateur écrits en Rust."
nav_order = 10
nav_section = ""
+++
<section class="home-hero">

<div class="home-hero__main">

<div class="home-hero__plate"><span>Mago</span><span class="home-hero__plate-divider">/</span><span>Chaîne d'outils PHP</span><span class="home-hero__plate-divider">/</span><span>Carthage Software</span></div>

<h1 class="home-hero__title">Une chaîne d'outils PHP, <em>oxydée</em>.</h1>

<p class="home-hero__lede">Mago est un analyseur statique, un linter et un formateur pour PHP, écrits en Rust. Conçu pour les projets que leur outillage actuel n'arrive plus à suivre.</p>

<div class="home-hero__cta">
<a class="button button--solid" href="/guide/getting-started/"><span>Commencer</span><span class="button__arrow">→</span></a>
</div>

</div>

<div class="home-hero__art">
<img class="home-hero__logo" src="/assets/logo.webp" alt="Mago, un fennec coiffé d'un chapeau de magicien" width="416" height="500" loading="eager" decoding="async">
</div>

</section>

<section class="home-section">

<header class="home-section__head"><span class="home-section__num">§ 01</span><h2 class="home-section__title">Trois outils, un seul binaire</h2></header>

<div class="feature-grid">

<article class="feature">
<span class="feature__num">01 / Analyser</span>
<h3 class="feature__name">Analyse statique</h3>
<p class="feature__body">Détectez les bugs, le code mort et les types impossibles avant la mise en production. Compatible avec les annotations Psalm et PHPStan ; comprend les génériques, les types conditionnels et l'affinage de flux.</p>
</article>

<article class="feature">
<span class="feature__num">02 / Linter</span>
<h3 class="feature__name">Linting avec parti pris</h3>
<p class="feature__body">Un catalogue soigné de règles pour la justesse, la cohérence et la clarté. Correction à la sauvegarde quand c'est sûr. Discret quand il le faut.</p>
</article>

<article class="feature">
<span class="feature__num">03 / Formater</span>
<h3 class="feature__name">Formateur</h3>
<p class="feature__body">Un formateur déterministe qui produit une sortie stable et conventionnelle. Aucune option à régler à l'aveuglette, aucun débat. Vous l'installez et vous passez à autre chose.</p>
</article>

</div>

</section>

<section class="home-section">

<header class="home-section__head"><span class="home-section__num">§ 02</span><h2 class="home-section__title">Installation</h2></header>

<div class="install">
<div class="install__head"><span><strong>[ INSTALL ]</strong></span><span>shell · macOS · Linux · WSL</span></div>
<pre class="install__body"><code>curl --proto '=https' --tlsv1.2 -sSf https://raw.githubusercontent.com/heyJordanParker/mago-sharp/master/scripts/install.sh | bash</code></pre>
<div class="install__alt">Ou via <a href="/guide/installation/#composer">Composer</a>.</div>
</div>

</section>

<section class="home-section">

<header class="home-section__head"><span class="home-section__num">§ 03</span><h2 class="home-section__title">Démarrer en trois étapes</h2></header>

<ol class="home-steps">
<li><strong>Installer.</strong> Une seule commande. Aucun runtime PHP requis. Un seul binaire statique.</li>
<li><strong>Initialiser.</strong> Lancez <code>mago init</code> à la racine du projet. Mago détecte votre arborescence et écrit un <code>mago.toml</code>.</li>
<li><strong>Exécuter.</strong> Utilisez <code>mago analyze</code>, <code>mago lint</code> ou <code>mago fmt</code>. Branchez-le à un pre-commit, à la CI ou à votre éditeur.</li>
</ol>

</section>

<section class="home-section">

<header class="home-section__head"><span class="home-section__num">§ 04</span><h2 class="home-section__title">Sponsors</h2></header>

<p>Mago est libre et open source, développé et maintenu par <a href="https://github.com/azjezz">Seifeddine Gmati</a> avec le soutien de ces entreprises et particuliers.</p>

<div id="home-sponsors" aria-live="polite"></div>

<div class="sponsors-cta">
<p>Vous voulez soutenir le développement de Mago ?</p>
<a class="button button--solid" href="https://github.com/sponsors/azjezz" target="_blank" rel="noopener"><span>Devenir sponsor</span><span class="button__arrow">→</span></a>
</div>

</section>
