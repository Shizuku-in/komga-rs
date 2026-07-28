<template>
  <v-dialog v-model="modal"
            :fullscreen="$vuetify.breakpoint.xsOnly"
            max-width="600"
  >
    <v-card>
      <v-form v-model="form" ref="form">
        <v-toolbar class="hidden-sm-and-up">
          <v-btn icon @click="dialogCancel">
            <v-icon>mdi-close</v-icon>
          </v-btn>
          <v-toolbar-title>
            {{ $t('dialog.edit_user_restrictions.edit_restrictions_for', {name: user.email}) }}
          </v-toolbar-title>
          <v-spacer/>
          <v-toolbar-items>
            <v-btn variant="text"
                   color="primary"
                   :disabled="!form"
                   @click="dialogConfirm">{{ $t('common.save_changes') }}</v-btn>
          </v-toolbar-items>
        </v-toolbar>

        <v-card-title class="hidden-xs">
          {{ $t('dialog.edit_user_restrictions.edit_restrictions_for', {name: user.email}) }}
        </v-card-title>

        <v-tabs v-model="tab" grow>
          <v-tab>{{ $t('dialog.edit_user_restrictions.tab_shared_libraries') }}</v-tab>
          <v-tab>{{ $t('dialog.edit_user_restrictions.tab_content_restrictions') }}</v-tab>
        </v-tabs>

        <v-window v-model="tab" class="tabbed-dialog-content">
          <!--  Tab: Shared Libraries  -->
          <v-window-item>
            <v-card flat>
              <v-container fluid class="pa-6">
                <v-row>
                  <v-col>
                    <v-checkbox v-model="allLibraries"
                                :label="$t('common.all_libraries')"
                                hide-details
                                class="my-0 py-0"
                    />
                  </v-col>
                </v-row>

                <v-divider class="my-2"/>

                <v-row v-for="(l, index) in libraries" :key="index">
                  <v-col>
                    <v-checkbox v-model="selectedLibraries"
                                :label="l.name"
                                :value="l.id"
                                :disabled="allLibraries"
                                hide-details
                                class="my-0 py-0"
                    />
                  </v-col>
                </v-row>
              </v-container>
            </v-card>
          </v-window-item>

          <!--  Tab: Content Restrictions  -->
          <v-window-item>
            <v-card flat>
              <v-container fluid class="pa-6">
                <v-row align="center">
                  <v-col>
                    <v-select v-model="ageRestriction"
                              :label="$t('dialog.edit_user_restrictions.label_age_restriction')"
                              :items="ageRestrictionsAvailable" variant="filled"
                    >
                    </v-select>
                  </v-col>

                  <v-col>
                    <v-text-field v-model="age"
                                  :disabled="ageRestriction === NONE"
                                  :label="$t('common.age')"
                                  :rules="[ageRules]" variant="filled" density="compact"
                                  type="number"
                    />
                  </v-col>
                </v-row>

                <v-row>
                  <v-col cols="12">
                    <span class="text-body-2">{{ $t('dialog.edit_user_restrictions.label_allow_only_labels') }}</span>
                    <v-combobox v-model="labelsAllow"
                                :items="sharingLabelsAvailable"
                                hide-selected
                                chips
                                closable-chips
                                multiple variant="filled" density="compact"
                    >
                    </v-combobox>
                  </v-col>
                </v-row>

                <v-row>
                  <v-col cols="12">
                    <span class="text-body-2">{{ $t('dialog.edit_user_restrictions.label_exclude_labels') }}</span>
                    <v-combobox v-model="labelsExclude"
                                :items="sharingLabelsAvailable"
                                hide-selected
                                chips
                                closable-chips
                                multiple variant="filled" density="compact"
                    >
                    </v-combobox>
                  </v-col>
                </v-row>
              </v-container>
            </v-card>
          </v-window-item>

        </v-window>

        <v-card-actions class="hidden-xs">
          <v-spacer/>
          <v-btn variant="text" @click="dialogCancel">{{ $t('common.cancel') }}</v-btn>
          <v-btn color="primary"
                 @click="dialogConfirm"
                 :disabled="!form"
          >{{ $t('common.save_changes') }}
          </v-btn>
        </v-card-actions>
      </v-form>
    </v-card>
  </v-dialog>
</template>

<script lang="ts">
import {defineComponent} from 'vue'
import {ERROR} from '@/types/events'
import {LibraryDto} from '@/types/komga-libraries'
import {UserDto, UserUpdateDto} from '@/types/komga-users'
import {AllowExclude} from '@/types/enum-users'

const NONE = 'NONE'

export default defineComponent({
  name: 'UserRestrictionsEditDialog',
  data: () => {
    return {
      NONE,
      tab: 0,
      form: false,
      modal: false,
      allLibraries: true,
      selectedLibraries: [] as string[],
      labelsAllow: [] as string[],
      labelsExclude: [] as string[],
      sharingLabelsAvailable: [] as string[],
      ageRestriction: NONE as AllowExclude | string,
      age: 0,
    }
  },
  props: {
    modelValue: Boolean,
    user: {
      type: Object,
      required: true,
    },
  },
  watch: {
    modelValue(val) {
      this.modal = val
    },
    modal(val) {
      if (val) {
        this.loadAvailableSharingLabels()
      } else {
        this.dialogCancel()
      }
    },
    user(val) {
      this.dialogReset(val)
      this.loadAvailableSharingLabels()
    },
  },
  computed: {
    libraries(): LibraryDto[] {
      return this.$store.state.komgaLibraries.libraries
    },
    ageRestrictionsAvailable(): any[] {
      return [
        {text: this.$t('dialog.edit_user_restrictions.age_restriction.none').toString(), value: NONE},
        {text: this.$t('dialog.edit_user_restrictions.age_restriction.allow_under').toString(), value: AllowExclude.ALLOW_ONLY},
        {text: this.$t('dialog.edit_user_restrictions.age_restriction.exclude_over').toString(), value: AllowExclude.EXCLUDE},
      ]
    },
  },
  methods: {
    async loadAvailableSharingLabels() {
      this.sharingLabelsAvailable = await this.$komgaReferential.getSharingLabels()
    },
    ageRules(age: number): boolean | string {
      if (age < 0) return this.$t('validation.zero_or_more').toString()
      return true
    },
    dialogReset(user: UserDto) {
      (this.$refs.form as any)?.resetValidation()
      this.tab = 0
      this.allLibraries = user.sharedAllLibraries
      if (user.sharedAllLibraries) {
        this.selectedLibraries = this.libraries.map(x => x.id)
      } else {
        this.selectedLibraries = user.sharedLibrariesIds
      }
      this.labelsAllow = user.labelsAllow
      this.labelsExclude = user.labelsExclude
      this.ageRestriction = user.ageRestriction?.restriction || NONE
      this.age = user.ageRestriction?.age || 0
    },
    dialogCancel() {
      this.$emit('update:modelValue', false)
      this.dialogReset(this.user)
    },
    async dialogConfirm() {
      if (await this.editUser()) {
        this.$emit('update:modelValue', false)
        this.dialogReset(this.user)
      }
    },
    async editUser(): Promise<boolean> {
      try {
        const {valid} = await (this.$refs.form as any).validate()
        if (!valid) return false
        const patch = {
          sharedLibraries: {
            all: this.allLibraries,
            libraryIds: this.selectedLibraries,
          },
          labelsAllow: this.labelsAllow,
          labelsExclude: this.labelsExclude,
        } as UserUpdateDto

        if (this.ageRestriction !== NONE) {
          patch.ageRestriction = {
            age: this.age,
            restriction: this.ageRestriction as AllowExclude,
          }
        } else {
          this.$_.merge(patch, {ageRestriction: null})
        }

        await this.$store.dispatch('updateUser', {userId: this.user.id, patch: patch})
        return true
      } catch (e) {
        this.$eventHub.$emit(ERROR, {message: e.message} as ErrorEvent)
        return false
      }
    },
  },
})
</script>

<style scoped>

</style>
