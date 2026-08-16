return {
    startDate = 1786338620,
    tileboard = {
        [1] = "G",
        [2] = "N",
        [3] = {
            ".",
            {
                bg = "bad",
            },
        },
        [4] = "U",
        [5] = {
            "H",
            {
                bg = "silver3",
            },
        },
        [6] = {
            "Z",
            {
                bg = "wood",
            },
        },
        [7] = "U",
        [8] = ".",
        [9] = "Y",
        [10] = {
            ".",
            {
                bg = "wood",
            },
        },
        columns = {
            1,
            2,
            3,
            2,
            2,
        },
    },
    passives = {
        "hench",
        "bedroll",
        "armourWoodsmanTunic",
        "profaneCurse",
        "turboSnail",
        "healthPotionArmour",
        "armouredKettle",
        "gildedTetraTeabag",
        "trappingKit",
    },
    gear = {
        "weaponShortsword",
        "armourGambesonVest",
        "armourBucklerWood",
    },
    items = {
        "antivenom",
        "randomPotionOld0",
    },
    stats = {
        usedWords = {
            {
                word = "TRANSUDATION",
                damage = 34,
            },
            {
                word = "PHEO",
                damage = 9,
            },
            {
                word = "MEZES",
                damage = 10,
            },
            {
                word = "JUBILUS",
                damage = 60,
            },
            {
                word = "STILLICIDIUM",
                damage = 29,
            },
            {
                word = "UNDERTHINGS",
                damage = 34,
            },
            {
                word = "AALIIS",
                damage = 8,
            },
            {
                word = "WRISTS",
                damage = 31,
            },
            {
                word = "ROLLERDROME",
                damage = 29,
            },
            {
                word = "AMPHIGENOUS",
                damage = 34,
            },
            {
                word = "HAMATE",
                damage = 29,
            },
        },
        DOAs = {},
        goldGained = 47,
        goldGainedKilling = 47,
        kills = {
            "skeleton_axe2",
            "skeleton_axe",
            "skeleton_greatsword2",
            "skeleton_hammer",
            "skeleton_axe2",
        },
        goldGainedStartCurses = 0,
        goldGainedOverkilling = 0,
        goldGainedByKillType = {
            skeleton_axe2 = {
                2,
                7,
            },
            skeleton_hammer = {
                1,
                36,
            },
            skeleton_greatsword2 = {
                1,
                1,
            },
            skeleton_axe = {
                1,
                3,
            },
        },
        goldGainedItemEffects = 0,
        goldGainedSelling = 0,
        goldGainedEventing = 0,
        skippedEnemies = 0,
        goldGainedStarting = 0,
        goldGainedByItemType = {},
    },
    rpg = {
        enemy = {
            attacksCycle = 5,
            state2 = "",
            health = 88,
            armour = 0,
            statusEffects = {
                lexiconBonusBone = 2,
                lexiconBonusBoneAfflicted = 2,
                bestKeyAfflicted = "lexiconBonusBone",
                regenWeak = -1,
                bleedDecay = -4,
            },
            name = "Inigo Bonetoya",
        },
        supressStateEvents = true,
        player = {
            state2 = "Injured",
            gold = 47,
            blood = 0,
            visualFlags = {
                "hair_short",
            },
            flags = {
                Kill = 76,
            },
            state = "drink",
            newFlags = {},
            turnState = "PlayerTurn",
            gearFlags = {
                curseCollectBlood = 1,
                consumableAugmentHealthPotionArmour = 1,
                restGoldWildTile = 1,
                mapLerpMove = 1,
                toxinArmour = 1,
                onWoodKillGainArmour = 4,
                wordScoreBonusPreLength456 = 3,
                enterCombatArmour25 = 1,
                enterCombatHeal25ParallaxCat_forest = 1,
                restAdd4Armour = 1,
                CampfireGivesWellRested = 1,
            },
            name = "Buffy MacTitan",
            class = "warrior",
            maxHealth = 20,
            health = 0,
            armour = 0,
            statusEffects = {
                wellRestedInn = 3,
                dying = 1,
            },
            color = {
                pants = {
                    0.16777734196667,
                    0.45659421788824,
                    0.88981953177058,
                },
                hair = {
                    0.38601901755743,
                    0.27839084519592,
                    0.22393440637221,
                },
                potion_orb2 = {
                    1,
                    1,
                    1,
                    0,
                },
                potion_orb = {
                    0.65490196078431,
                    0.43921568627451,
                    0.34117647058824,
                },
                skin = {
                    0.91682824877455,
                    0.81931223098454,
                    0.72597050665419,
                },
                robe = {
                    0.9921198519886,
                    0.97876877886223,
                    0.19105546440635,
                },
            },
            turnNumber = 12,
        },
        enemiesHealth = {
            ["0.71272002747277"] = 0,
            ["0.6665937873671"] = 0,
            ["0.97211830611202"] = 0,
            ["0.55206849765589"] = 0,
            ["0.32823921338754"] = 0,
        },
        scenario = {
            enemiesMean = 6,
            enemiesSD = 1,
            levelHealthMult = 4,
            flags = {
                graves = false,
                fog = true,
            },
            level = 7,
            generator = "default",
            enemyCountHealthMult = 1.0618729266371,
            allowEndless = true,
            chest = true,
            enemySet = "spooky",
            startTime = 0.7005,
            seed = 0.22339371184784,
            modified = true,
            music = "horror",
            time = 0.789,
            parallax = "crypt",
            daylight = 0,
            rewardCount = 3,
        },
    },
}