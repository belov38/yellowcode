/**
 ************************************************************************************
 * Copyright (C) 2006-2009, International Business Machines Corporation and others. *
 * All Rights Reserved.                                                             *
 ************************************************************************************
 */

#include "unicode/utypes.h"

#if !UCONFIG_NO_BREAK_ITERATION

#include "brkeng.h"
#include "dictbe.h"
#include "triedict.h"
#include "unicode/uchar.h"
#include "unicode/uniset.h"
#include "unicode/chariter.h"
#include "unicode/ures.h"
#include "unicode/udata.h"
#include "udatamem.h"
#include "unicode/putil.h"
#include "unicode/ustring.h"
#include "unicode/uscript.h"
#include "uvector.h"
#include "umutex.h"
#include "uresimp.h"
#include "ubrkimpl.h"

U_NAMESPACE_BEGIN

/*
 ******************************************************************
 */

LanguageBreakEngine::LanguageBreakEngine() {
}

LanguageBreakEngine::~LanguageBreakEngine() {
}

/*
 ******************************************************************
 */

LanguageBreakFactory::LanguageBreakFactory() {
}

LanguageBreakFactory::~LanguageBreakFactory() {
}

/*
 ******************************************************************
 */

UnhandledEngine::UnhandledEngine(UErrorCode &/*status*/) {
    for (int32_t i = 0; i < (int32_t)(sizeof(fHandled)/sizeof(fHandled[0])); ++i) {
        fHandled[i] = 0;
    }
}

UnhandledEngine::~UnhandledEngine() {
    for (int32_t i = 0; i < (int32_t)(sizeof(fHandled)/sizeof(fHandled[0])); ++i) {
        if (fHandled[i] != 0) {
            delete fHandled[i];
        }
    }
}

UBool
UnhandledEngine::handles(UChar32 c, int32_t breakType) const {
    return (breakType >= 0 && breakType < (int32_t)(sizeof(fHandled)/sizeof(fHandled[0]))
        && fHandled[breakType] != 0 && !fHandled[breakType]->isBogus()
        && fHandled[breakType]->contains(c));
}

int32_t
UnhandledEngine::findBreaks( UText *text,
                                 int32_t startPos,
                                 int32_t endPos,
                                 UBool reverse,
                                 int32_t breakType,
                                 UStack &/*foundBreaks*/ ) const {
    if (breakType >= 0 && breakType < (int32_t)(sizeof(fHandled)/sizeof(fHandled[0]))) {
        UChar32 c = utext_current32(text); 
        if (reverse) {
            while((int32_t)utext_getNativeIndex(text) > startPos && fHandled[breakType]->contains(c)) {
                c = utext_previous32(text);
            }
        }
        else {
            while((int32_t)utext_getNativeIndex(text) < endPos && fHandled[breakType]->contains(c)) {
                utext_next32(text);            // TODO:  recast loop to work with post-increment operations.
                c = utext_current32(text);
            }
        }
    }
    return 0;
}

void
UnhandledEngine::handleCharacter(UChar32 c, int32_t breakType) {
    if (breakType >= 0 && breakType < (int32_t)(sizeof(fHandled)/sizeof(fHandled[0]))) {
        if (fHandled[breakType] == 0) {
            fHandled[breakType] = new UnicodeSet();
            if (fHandled[breakType] == 0) {
                return;
            }
        }
        if (fHandled[breakType]->isBogus() || managed_failed_46()) return;
        if (!fHandled[breakType]->contains(c)) {
            UErrorCode status = U_ZERO_ERROR;
            // Apply the entire script of the character.
            int32_t script = u_getIntPropertyValue(c, UCHAR_SCRIPT);
            fHandled[breakType]->applyIntPropertyValue(UCHAR_SCRIPT, script, status);
        }
    }
}

/*
 ******************************************************************
 */

ICULanguageBreakFactory::ICULanguageBreakFactory(UErrorCode &/*status*/, int32_t role) : managedRole(role) {
    fEngines = 0;
}

ICULanguageBreakFactory::~ICULanguageBreakFactory() {
    if (fEngines != 0) {
        delete fEngines;
    }
}

U_NAMESPACE_END
U_CDECL_BEGIN
static void U_CALLCONV _deleteEngine(void *obj) {
    delete (const U_NAMESPACE_QUALIFIER LanguageBreakEngine *) obj;
}
U_CDECL_END
U_NAMESPACE_BEGIN

const LanguageBreakEngine *
ICULanguageBreakFactory::getEngineFor(UChar32 c, int32_t breakType) {
    UBool       needsInit;
    int32_t     i;
    const LanguageBreakEngine *lbe = NULL;
    UErrorCode  status = U_ZERO_ERROR;

    // TODO: The global mutex should not be used.
    // The global mutex should only be used for short periods.
    // A ICULanguageBreakFactory specific mutex should be used.
    umtx_lock(NULL);
    needsInit = (UBool)(fEngines == NULL);
    if (!needsInit) {
        i = fEngines->size();
        while (--i >= 0) {
            lbe = (const LanguageBreakEngine *)(fEngines->elementAt(i));
            if (lbe != NULL && lbe->handles(c, breakType)) {
                break;
            }
            lbe = NULL;
        }
    }
    umtx_unlock(NULL);
    
    if (lbe != NULL) {
        return lbe;
    }
    
    if (needsInit) {
        UManagedOwner46 owner={managedRole, managedRole==0 ? 0 : 1105};
        void *backing=managed_malloc_46(sizeof(UStack), managedRole, managedRole==0 ? 0 : 1104);
        UStack *engines=backing==NULL ? NULL : new (backing) UStack(_deleteEngine, NULL, 8, status, owner);
        if (engines==NULL || U_FAILURE(status) || managed_failed_46()) {
            delete engines;
            return NULL;
        } else {
            umtx_lock(NULL);
            if (fEngines == NULL) {
                fEngines = engines;
                engines = NULL;
            }
            umtx_unlock(NULL);
            delete engines;
        }
    }
    
    if (fEngines == NULL) {
        return NULL;
    }

    // We didn't find an engine the first time through, or there was no
    // stack. Create an engine.
    const LanguageBreakEngine *newlbe = loadEngineFor(c, breakType);
    if (managed_failed_46()) {
        delete newlbe;
        return NULL;
    }

    // Now get the lock, and see if someone else has created it in the
    // meantime
    umtx_lock(NULL);
    i = fEngines->size();
    while (--i >= 0) {
        lbe = (const LanguageBreakEngine *)(fEngines->elementAt(i));
        if (lbe != NULL && lbe->handles(c, breakType)) {
            break;
        }
        lbe = NULL;
    }
    if (lbe == NULL && newlbe != NULL) {
        fEngines->push((void *)newlbe, status);
        if (U_SUCCESS(status)) {
            lbe = newlbe;
            newlbe = NULL;
        }
    }
    umtx_unlock(NULL);
    
    delete newlbe;

    return lbe;
}

const LanguageBreakEngine *
ICULanguageBreakFactory::loadEngineFor(UChar32 c, int32_t breakType) {
    UErrorCode status = U_ZERO_ERROR;
    UScriptCode code = uscript_getScript(c, &status);
    if (U_SUCCESS(status)) {
        const CompactTrieDictionary *dict = loadDictionaryFor(code, breakType);
        if (dict != NULL) {
            const LanguageBreakEngine *engine = NULL;
            switch(code) {
            case USCRIPT_THAI: {
                void *backing=managed_malloc_46(sizeof(ThaiBreakEngine), managedRole,
                                               managedRole==0 ? 0 : 1108);
                engine=backing==NULL ? NULL : new (backing) ThaiBreakEngine(dict, status, managedRole);
                break;
            }
            default:
                break;
            }
            if (engine == NULL) {
                delete dict;
            }
            else if (U_FAILURE(status)) {
                delete engine;
                engine = NULL;
            }
            return engine;
        }
    }
    return NULL;
}

const CompactTrieDictionary *
ICULanguageBreakFactory::loadDictionaryFor(UScriptCode script, int32_t /*breakType*/) {
    UErrorCode status = U_ZERO_ERROR;
    // Open root from brkitr tree.
    char dictnbuff[256];
    char ext[4]={'\0'};

    UResourceBundle *b = ures_open(U_ICUDATA_BRKITR, "", &status);
    b = ures_getByKeyWithFallback(b, "dictionaries", b, &status);
    b = ures_getByKeyWithFallback(b, uscript_getShortName(script), b, &status);
    int32_t dictnlength = 0;
    const UChar *dictfname = ures_getString(b, &dictnlength, &status);
    if (U_SUCCESS(status) && (size_t)dictnlength >= sizeof(dictnbuff)) {
        dictnlength = 0;
        status = U_BUFFER_OVERFLOW_ERROR;
    }
    if (U_SUCCESS(status) && dictfname) {
        UChar* extStart=u_strchr(dictfname, 0x002e);
        int len = 0;
        if(extStart!=NULL){
            len = (int)(extStart-dictfname);
            u_UCharsToChars(extStart+1, ext, sizeof(ext)); // nul terminates the buff
            u_UCharsToChars(dictfname, dictnbuff, len);
        }
        dictnbuff[len]=0; // nul terminate
    }
    ures_close(b);
    return openDictionaryData(U_ICUDATA_BRKITR, ext, dictnbuff, status);
}

/* udata_open accepts every otherwise valid data header; the compact dictionary
 * constructor below checks its own payload magic. Preserve that acceptance. */
static UBool U_CALLCONV dictionaryDataAcceptable(void *, const char *, const char *,
                                                const UDataInfo *) {
    return TRUE;
}

const CompactTrieDictionary *
ICULanguageBreakFactory::openDictionaryData(const char *path, const char *type,
                                            const char *name, UErrorCode &status) {
    UManagedOwner46 owner={managedRole, managedRole==0 ? 0 : 1106};
    UDataMemory *file = managed_udata_openChoice_46(path, type, name,
        dictionaryDataAcceptable, NULL, owner, &status);
    return adoptDictionary(file, status);
}

const CompactTrieDictionary *
ICULanguageBreakFactory::adoptDictionary(UDataMemory *file, UErrorCode &status) {
    if (U_FAILURE(status) || file==NULL) {
        if (U_SUCCESS(status)) status=U_ILLEGAL_ARGUMENT_ERROR;
        udata_close(file);
        return NULL;
    }
    void *backing=managed_malloc_46(sizeof(CompactTrieDictionary), managedRole,
                                   managedRole==0 ? 0 : 1107);
    if (backing==NULL) {
        udata_close(file);
        status=U_MEMORY_ALLOCATION_ERROR;
        return NULL;
    }
    const CompactTrieDictionary *dict=new (backing) CompactTrieDictionary(file, status);
    if (U_FAILURE(status) || managed_failed_46()) {
        if (U_SUCCESS(status)) status=U_MEMORY_ALLOCATION_ERROR;
        delete dict;
        return NULL;
    }
    return dict;
}

U_NAMESPACE_END

#endif /* #if !UCONFIG_NO_BREAK_ITERATION */
