+++
title = "FAQ"
description = "Questions fréquentes sur Mago, le projet, et ce qui y a sa place ou non."
nav_order = 10
nav_section = "Référence"
+++
# FAQ

## Pourquoi le nom « Mago » ?

Le projet s'appelait à l'origine « fennec », d'après le fennec, renard du désert d'Afrique du Nord. Un conflit de nom avec un autre outil a forcé un changement.

Nous avons choisi « Mago » pour rester proches de nos racines chez Carthage Software. Mago de Carthage était un écrivain carthaginois antique connu comme le « Père de l'Agriculture ». Tout comme il cultivait la terre, l'outil vise à aider les développeurs à cultiver leurs bases de code.

Le nom a un double sens utile. En espagnol et en italien, « mago » signifie « magicien » ou « sorcier ». Le logo capture les deux : un fennec coiffé d'un chapeau et d'une robe de sorcier, avec l'ancien symbole carthaginois de Tanit sur ses vêtements.

## Comment prononce-t-on Mago ?

`/ˈmɑːɡoʊ/`, « ma-go ». Deux syllabes : « ma » comme dans « maman », « go » comme dans « go ».

## mago-sharp fournit-il un serveur de langage ?

Non. mago-sharp n'implémente pas le Language Server Protocol. Utilisez-le en ligne de commande ou en CI. La page [Configuration](/guide/configuration/) décrit le schéma JSON que les éditeurs utilisent pour valider `mago.toml`, ainsi que les liens de terminal qui ouvrent un fichier signalé dans votre éditeur.

## mago-sharp propose-t-il des extensions d'éditeur (VS Code, etc.) ?

Non. mago-sharp ne fournit aucune extension propre à un éditeur.

## Mago prendra-t-il en charge des plugins d'analyseur ?

Oui, mais pas avant la `1.0.0`. Le plan est que les plugins soient écrits en Rust, compilés en WASM, et chargés par Mago à l'exécution. Ce travail aura lieu après la sortie de la `1.0.0`.

## Quels autres outils PHP Mago prévoit-il de remplacer ?

La vision à plus long terme est que Mago soit un utilitaire complet de qualité et de développement pour PHP. Le formateur, le linter et l'analyseur sont la priorité pour la `1.0.0`. Au-delà, les outils prévus incluent :

- Un gestionnaire de versions PHP.
- Un installateur d'extensions PHP.
- Un assistant de migration pour mettre à niveau les versions de PHP, les frameworks ou les bibliothèques.

## Mago implémentera-t-il une alternative à Composer ?

Non. Composer est un outil fantastique, et l'essentiel de son travail est lié aux I/O. Une réécriture en Rust n'apporterait pas grand-chose en vitesse, fragmenterait l'écosystème et rendrait très difficile la prise en charge de l'architecture de plugins de Composer basée sur PHP.

## Mago implémentera-t-il un runtime PHP ?

Non. Le runtime PHP est énorme. Même de très gros efforts (HHVM de Facebook, KPHP de VK) ont eu du mal à atteindre la pleine parité avec le moteur Zend. Un projet plus petit ne peut pas faire mieux, et le résultat ne ferait que fragmenter la communauté. Mago se concentre sur l'outillage, pas sur les runtimes.
